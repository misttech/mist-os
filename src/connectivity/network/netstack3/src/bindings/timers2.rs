// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::BinaryHeap,
    fmt::Debug,
    sync::{
        atomic::{self, AtomicI64},
        Arc,
    },
    task::Poll,
};

use async_utils::futures::YieldToExecutorOnce;
use fuchsia_async as fasync;
use futures::{future::FusedFuture, Future, FutureExt, StreamExt as _};
use netstack3_core::sync::Mutex as CoreMutex;
use tracing::{trace, warn};

use crate::bindings::util::{NeedsDataNotifier, NeedsDataWatcher};

pub(crate) use scheduled_instant::ScheduledInstant;

/// A special time that is used as a marker for an unscheduled timer.
const UNSCHEDULED_SENTINEL: fasync::Time = fasync::Time::INFINITE;

/// The maximum number of timers that will fire without yielding to the
/// executor.
///
/// This allows us to yield time to other tasks if there's a thundering herd of
/// timers firing at the same time.
const YIELD_TIMER_COUNT: usize = 10;

pub(crate) struct TimerDispatcher<T> {
    inner: Arc<CoreMutex<TimerDispatcherInner<T>>>,
}

impl<T> Default for TimerDispatcher<T> {
    fn default() -> Self {
        let notifier = NeedsDataNotifier::default();
        let watcher = notifier.watcher();
        Self {
            inner: Arc::new(CoreMutex::new(TimerDispatcherInner {
                heap: BinaryHeap::new(),
                notifier: Some(notifier),
                watcher: Some(watcher),
            })),
        }
    }
}

impl<T: Clone + Send + Sync + Debug + 'static> TimerDispatcher<T> {
    /// Spawns a worker for this `TimerDispatcher`.
    ///
    /// The worker task will only end when `close` is called on this
    /// `TimerDispatcher`.
    ///
    /// `handler` is called whenever a timer is fired.
    ///
    /// # Panics
    ///
    /// Panics if this `TimerDispatcher` has already been spawned.
    pub(crate) fn spawn<H: FnMut(T) + Send + Sync + 'static>(
        &self,
        handler: H,
    ) -> fasync::Task<()> {
        let watcher = self.inner.lock().watcher.take().expect("timer dispatcher already spawned");
        fasync::Task::spawn(Self::worker(handler, watcher, Arc::clone(&self.inner)))
    }

    /// Creates a new timer with identifier `dispatch` on this `dispatcher`.
    pub(crate) fn new_timer(&self, dispatch: T) -> Timer<T> {
        Timer {
            heap: Arc::clone(&self.inner),
            state: Arc::new(TimerState {
                dispatch,
                scheduled: AtomicI64::new(UNSCHEDULED_SENTINEL.into_nanos()),
            }),
            _no_clone: NoCloneGuard,
        }
    }

    /// Stops this timer dispatcher.
    ///
    /// This causes a task previously returned by [`TimerDispatcher::spawn`] to
    /// complete.
    ///
    /// # Panics
    ///
    /// If `stop` was already called.
    pub(crate) fn stop(&self) {
        assert!(self.inner.lock().notifier.take().is_some(), "dispatcher already closed");
    }

    async fn worker<H: FnMut(T)>(
        mut handler: H,
        mut watcher: NeedsDataWatcher,
        inner: Arc<CoreMutex<TimerDispatcherInner<T>>>,
    ) {
        let mut timer = TimerWaiter::new();
        loop {
            let mut watcher_next = watcher.next().fuse();
            futures::select! {
                () = timer => (),
                w = watcher_next => {
                    match w {
                        Some(()) => (),
                        // Dispatcher is closed, break the loop.
                        None => break,
                    }
                }
            }

            let next_wakeup = Self::check_timer_heap(&mut handler, &*inner).await;

            trace!("next wakeup = {:?}", ScheduledInstant::new(next_wakeup));
            // Update our timer to wake at the next wake up time according to
            // the heap.
            timer.set(next_wakeup);

            // TODO(https://fxbug.dev/42083407): Tune the waiter collection
            // target heap size and GC the timer heap based on the number of
            // timers.
        }
        // Cleanup everything on the heap before returning.
        inner.lock().heap.clear();
    }

    /// Checks the timer heap, firing any elapsed timers.
    ///
    /// Returns the next wakeup time for the worker task.
    async fn check_timer_heap<H: FnMut(T)>(
        handler: &mut H,
        inner: &CoreMutex<TimerDispatcherInner<T>>,
    ) -> fasync::Time {
        let mut fired_count = 0;
        loop {
            let fire = {
                let mut guard = inner.lock();
                let TimerDispatcherInner { heap, notifier: _, watcher: _ } = &mut *guard;
                let Some(front) = heap.peek_mut() else {
                    // Nothing to wait for.
                    return UNSCHEDULED_SENTINEL;
                };
                if !front.should_fire_at(fasync::Time::now()) {
                    // Wait until the time at the front of the heap to fire.
                    return front.time;
                }
                // NB: This is an associated function, probably because
                // PeekMut implements Deref.
                let front = std::collections::binary_heap::PeekMut::pop(front);
                match front.try_fire() {
                    TryFireResult::Fire(f) => f,
                    TryFireResult::Ignore => continue,
                    TryFireResult::Reschedule(r) => {
                        heap.push(r);
                        continue;
                    }
                }
            };
            trace!("firing timer {fire:?}");
            handler(fire);
            fired_count += 1;
            if fired_count % YIELD_TIMER_COUNT == 0 {
                YieldToExecutorOnce::new().await;
            }
        }
    }
}

struct TimerDispatcherInner<T> {
    heap: BinaryHeap<TimerScheduledEntry<T>>,
    // Notifier is removed on close.
    notifier: Option<NeedsDataNotifier>,
    // Watcher is taken on spawn.
    watcher: Option<NeedsDataWatcher>,
}

/// A reusable struct to place a value and a timestamp in a [`BinaryHeap`].
///
/// Its `Ord` implementation is tuned to make [`BinaryHeap`] a min heap.
struct TimeAndValue<T> {
    time: fasync::Time,
    value: T,
}

// Boilerplate to implement a heap entry.
impl<T> PartialEq for TimeAndValue<T> {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time
    }
}

impl<T> Eq for TimeAndValue<T> {}

impl<T> Ord for TimeAndValue<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Note that we flip the argument order here to make the `BinaryHeap` a
        // min heap.
        Ord::cmp(&other.time, &self.time)
    }
}

impl<T> PartialOrd for TimeAndValue<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(Ord::cmp(self, other))
    }
}

/// An entry in the [`TimerDispatcher`] heap.
type TimerScheduledEntry<T> = TimeAndValue<Arc<TimerState<T>>>;

impl<T: Clone> TimerScheduledEntry<T> {
    /// Consumes the entry in an attempt to fire the timer.
    fn try_fire(self) -> TryFireResult<T> {
        let Self { value: timer_state, time: scheduled_for } = self;
        match timer_state.scheduled.compare_exchange(
            scheduled_for.into_nanos(),
            UNSCHEDULED_SENTINEL.into_nanos(),
            atomic::Ordering::SeqCst,
            atomic::Ordering::SeqCst,
        ) {
            // We may only fire the timer if the timer state is holding the same
            // value that it had when it was put into the heap.
            Ok(_) => TryFireResult::Fire(timer_state.dispatch.clone()),
            Err(next) => {
                let next = fasync::Time::from_nanos(next);
                if next > scheduled_for && next != UNSCHEDULED_SENTINEL {
                    // If the timer state is holding a time later than the
                    // originally scheduled, we need to put it back into the
                    // heap to be fired later.
                    TryFireResult::Reschedule(Self { value: timer_state, time: next })
                } else {
                    // If the timer state is holding a time before the one
                    // originally scheduled, this is a stale timer entry. We
                    // should ignore it.
                    TryFireResult::Ignore
                }
            }
        }
    }

    /// Returns whether this entry should fire for the current time.
    fn should_fire_at(&self, now: fasync::Time) -> bool {
        self.time <= now
    }
}

/// The result of operating a valid timer entry.
enum TryFireResult<T> {
    /// Fire this timer with the provided dispatch id.
    Fire(T),
    /// Timer entry is stale, ignore it.
    Ignore,
    /// Reschedule with this new entry.
    Reschedule(TimerScheduledEntry<T>),
}

/// The state kept in each timer.
struct TimerState<T> {
    /// Schedule holds the most up to date desired wake up time for a timer.
    ///
    /// This value is atomically read and compared to the cached time in
    /// [`TimeAndValue`] to validate the desired wake up time hasn't changed.
    scheduled: AtomicI64,
    dispatch: T,
}

/// This type is preventing [`Timer`] from evolving poorly and deriving `Clone`.
struct NoCloneGuard;

pub(crate) struct Timer<T> {
    state: Arc<TimerState<T>>,
    heap: Arc<CoreMutex<TimerDispatcherInner<T>>>,
    /// This type must not become Clone, see explanation in [`Timer::schedule`].
    _no_clone: NoCloneGuard,
}

impl<T> Debug for Timer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Many structs in core that use timers are self-referential so we need
        // to be careful in the Debug impl here, especially around the opaque
        // type T.
        let Self { state, heap: _, _no_clone: _ } = self;
        let TimerState { scheduled, dispatch: _ } = &**state;
        let scheduled = ScheduledInstant::new(fasync::Time::from_nanos(
            scheduled.load(atomic::Ordering::Relaxed),
        ));
        f.debug_struct("Timer").field("scheduled", &scheduled).finish_non_exhaustive()
    }
}

impl<T> Drop for Timer<T> {
    fn drop(&mut self) {
        let _: Option<ScheduledInstant> = self.cancel();
    }
}

impl<T> Timer<T> {
    pub(crate) fn schedule(&mut self, when: fasync::Time) -> Option<ScheduledInstant> {
        let prev = self.state.scheduled.swap(when.into_nanos(), atomic::Ordering::SeqCst);
        let prev = fasync::Time::from_nanos(prev);

        // CRITICAL SECTION HERE.
        //
        // A timer *cannot* be cloned because there must be only one instance of
        // if that can schedule; the cloned state may only be used by the heap
        // to invalidate, by setting the scheduled time to infinite future.
        //
        // If we raced with a different scheduling operation here the simple
        // check to see if the previous value is in the future could cause
        // problems.
        //
        // Given no cloning, the race here is prevented because the timer heap
        // only writes infinite future and always checks it against the expected
        // scheduled time. So if this timer was in the heap and it just fired,
        // then the heap will take care of rescheduling it for the future if it
        // was pushed later in time or drop it if it was pushed earlier in time.
        // Those actions are compatible with what we do below: do nothing if
        // we're re-scheduling for a later moment in time and push back to the
        // heap otherwise.
        //
        // Note that cancelation will never hit this branch because it's the
        // maximum value.
        if prev > when {
            // If the new timer is for an earlier moment in time, then we need
            // to put this in the heap; the old entry will eventually be a
            // no-op.
            let mut guard = self.heap.lock();
            let wake_notifier = {
                let TimerDispatcherInner { heap, notifier, watcher: _ } = &mut *guard;
                if let Some(notifier) = notifier.as_ref() {
                    let front = heap.peek().map(|c| c.time);
                    heap.push(TimerScheduledEntry { time: when, value: Arc::clone(&self.state) });
                    // Wake the timer task whenever the front of the heap changes.
                    (front != heap.peek().map(|c| c.time)).then_some(notifier)
                } else {
                    // Avoid doing any work if the notifier is gone, that means
                    // the timer is going away.
                    warn!("TimerDispatcher is closed, timer will not fire");
                    None
                }
            };
            if let Some(notifier) = wake_notifier {
                notifier.schedule();
            }
        } else {
            // Updating the atomic is sufficient to reschedule a timer for a
            // later instant. When the entry is popped from the heap, the
            // updated value from `TimerState` informs the heap of the
            // reschedule.
        }
        ScheduledInstant::new(prev)
    }

    pub(crate) fn cancel(&mut self) -> Option<ScheduledInstant> {
        self.schedule(UNSCHEDULED_SENTINEL)
    }

    pub(crate) fn scheduled_time(&self) -> Option<ScheduledInstant> {
        ScheduledInstant::new(fasync::Time::from_nanos(
            self.state.scheduled.load(atomic::Ordering::SeqCst),
        ))
    }
}

const DEFAULT_TIMER_WAITER_TARGET_HEAP_SIZE: usize = 10;

/// This provides a means to reuse [`fasync::Timer`] across run loops of
/// [`TimerDispatcher`], minimizing reallocations and decreasing leaks.
///
/// A `fasync::Timer` allocates backing memory that is only freed when the time
/// it was created with expires.. `TimerWaiter` offers a self-contained reusable
/// collection of timers that it decides to use based on the desired scheduled
/// wait time.
///
/// It also works around the limitation that an `fasync::Timer` can only be
/// rescheduled after it has fired.
struct TimerWaiter {
    timers: BinaryHeap<TimeAndValue<fasync::Timer>>,
    selected: Option<TimeAndValue<fasync::Timer>>,
    wakeup: fasync::Time,
    /// The target heap size controls when we let go of expired fasync::Timers
    /// instead of reusing them.
    target_heap_size: usize,
}

impl TimerWaiter {
    /// Creates a new `TimerWaiter`.
    ///
    /// In the default state, `TimerWaiter` is scheduled for `INFINITE_FUTURE`.
    fn new() -> Self {
        Self {
            timers: Default::default(),
            selected: None,
            wakeup: UNSCHEDULED_SENTINEL,
            target_heap_size: DEFAULT_TIMER_WAITER_TARGET_HEAP_SIZE,
        }
    }

    /// Sets the time at which this future will resolve to ready.
    ///
    /// Note that an `INFINITE_FUTURE` wakeup time will cause `TimerWaiter` to
    /// behave like a terminated future, i.e., `FusedFuture::is_terminated` is
    /// `true` and polling it may panic.
    fn set(&mut self, new_wakeup: fasync::Time) {
        let Self { timers, selected, wakeup, target_heap_size: _ } = self;
        if std::mem::replace(wakeup, new_wakeup) == new_wakeup {
            return;
        }
        // If we have a new wakeup time, always give up on the currently
        // selected timer and push to the heap.
        if let Some(selected) = selected.take() {
            timers.push(selected);
        }

        // Don't use timers for waiting until infinity. Our `FusedFuture`
        // implementation will prevent this from being polled when that's the
        // case.
        if new_wakeup == UNSCHEDULED_SENTINEL {
            return;
        }

        let time_and_value = timers
            .peek_mut()
            .and_then(|front| {
                // This is a good candidate IFF the fasync time is scheduled
                // before our new wakeup time or it matches it exactly.
                (front.time <= new_wakeup)
                    .then(|| std::collections::binary_heap::PeekMut::pop(front))
            })
            .unwrap_or_else(|| {
                // If there are no timers available in the heap then we need to
                // allocate a new timer.
                TimeAndValue { time: new_wakeup, value: fasync::Timer::new(new_wakeup) }
            });

        *selected = Some(time_and_value);
    }

    fn maybe_return_fired_timer_to_pool(&mut self, entry: TimeAndValue<fasync::Timer>) {
        // Place the timer back in the heap if we're not over the threshold.
        //
        // We don't store anything special in the heap saying this timer has
        // already fired because fasync::Timer can be polled to discover that
        // immediately, and that's resolved in our polling implementation.
        // Whenever we observe `Ready` from the timer.
        if self.timers.len() < self.target_heap_size {
            self.timers.push(entry);
        }
    }
}

impl Future for TimerWaiter {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        let Self { timers: _, selected, wakeup, target_heap_size: _ } = &mut *self;
        // Must not poll without a selected timer.
        let mut taken_selected = selected.take().unwrap();
        match taken_selected.value.poll_unpin(cx) {
            Poll::Ready(()) => (),
            Poll::Pending => {
                // Place it back while pending.
                *selected = Some(taken_selected);
                return Poll::Pending;
            }
        }
        if *wakeup <= taken_selected.time {
            self.maybe_return_fired_timer_to_pool(taken_selected);
            return Poll::Ready(());
        }
        // If our selected timer was scheduled for earlier than our wakeup
        // reset the timer now that it fired and poll again.
        taken_selected.value.reset(*wakeup);
        taken_selected.time = *wakeup;

        match taken_selected.value.poll_unpin(cx) {
            Poll::Ready(()) => {
                self.maybe_return_fired_timer_to_pool(taken_selected);
                Poll::Ready(())
            }
            Poll::Pending => {
                // Still pending put it back.
                *selected = Some(taken_selected);
                Poll::Pending
            }
        }
    }
}

impl FusedFuture for TimerWaiter {
    fn is_terminated(&self) -> bool {
        self.selected.is_none()
    }
}

/// A separate module for [`ScheduledInstant`] so it can't be constructed
/// violating its invariants.
mod scheduled_instant {
    use crate::bindings::StackTime;

    use super::{fasync, UNSCHEDULED_SENTINEL};

    /// A time that stands as a witness for a valid schedule time.
    ///
    /// It can only be constructed with an [`fasync::Time`] that is not
    /// `INFINITE_FUTURE`.
    #[derive(Eq, PartialEq)]
    pub(crate) struct ScheduledInstant(fasync::Time);

    impl std::fmt::Debug for ScheduledInstant {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let Self(time) = self;
            f.debug_tuple("ScheduledInstant").field(&time.into_nanos()).finish()
        }
    }

    impl ScheduledInstant {
        /// Constructs a new `ScheduledInstant` if `time` is not
        /// `INFINITE_FUTURE`.
        pub(super) fn new(time: fasync::Time) -> Option<Self> {
            (time != UNSCHEDULED_SENTINEL).then_some(Self(time))
        }
    }

    impl From<ScheduledInstant> for fasync::Time {
        fn from(ScheduledInstant(value): ScheduledInstant) -> Self {
            value
        }
    }

    impl From<ScheduledInstant> for StackTime {
        fn from(value: ScheduledInstant) -> Self {
            Self(value.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::channel::mpsc;

    use crate::bindings::integration_tests::set_logger_for_test;

    use super::*;

    impl TimerWaiter {
        #[track_caller]
        fn assert_timers<I: IntoIterator<Item = fasync::Time>>(
            &self,
            expect_heap: I,
            expect_selected: Option<fasync::Time>,
        ) {
            let Self { timers, selected, wakeup: _, target_heap_size: _ } = self;
            // Collect times and compare sorted vecs.
            let mut expect_heap = expect_heap.into_iter().collect::<Vec<_>>();
            expect_heap.sort();
            let mut timers =
                timers.iter().map(|TimeAndValue { time, value: _ }| *time).collect::<Vec<_>>();
            timers.sort();
            assert_eq!(timers, expect_heap);
            assert_eq!(
                selected.as_ref().map(|TimeAndValue { time, value: _ }| *time),
                expect_selected
            );
        }
    }

    fn new_test_executor() -> fasync::TestExecutor {
        let executor = fasync::TestExecutor::new_with_fake_time();
        // Set to zero to simplify logs in tests.
        executor.set_fake_time(T0);
        executor
    }

    #[track_caller]
    fn run_in_executor<R, Fut: Future<Output = R>>(
        executor: &mut fasync::TestExecutor,
        f: Fut,
    ) -> R {
        futures::pin_mut!(f);
        match executor.run_until_stalled(&mut f) {
            Poll::Ready(r) => r,
            Poll::Pending => panic!("Executor stalled"),
        }
    }

    const T0: fasync::Time = fasync::Time::from_nanos(0);
    const T1: fasync::Time = fasync::Time::from_nanos(1);
    const T2: fasync::Time = fasync::Time::from_nanos(2);
    const T3: fasync::Time = fasync::Time::from_nanos(3);
    const T4: fasync::Time = fasync::Time::from_nanos(4);

    #[derive(Debug, Eq, PartialEq, Clone)]
    enum TimerId {
        A,
        B,
        C,
    }

    struct TestContext {
        dispatcher: TimerDispatcher<TimerId>,
        fired: mpsc::UnboundedReceiver<TimerId>,
        task: fasync::Task<()>,
    }

    impl TestContext {
        fn new() -> Self {
            let dispatcher = TimerDispatcher::default();
            let (sender, fired) = mpsc::unbounded();
            let task = dispatcher
                .spawn(move |t| sender.unbounded_send(t).expect("failed to send fired timer"));
            Self { dispatcher, fired, task }
        }

        fn heap_len(&self) -> usize {
            self.dispatcher.inner.lock().heap.len()
        }

        async fn shutdown(self) {
            let Self { dispatcher, fired, task } = self;
            dispatcher.stop();
            task.await;
            assert_eq!(fired.collect::<Vec<_>>().await, vec![], "unacknowledged fired timers");
        }

        fn shutdown_in_executor((this, mut executor): (Self, fasync::TestExecutor)) {
            run_in_executor(&mut executor, this.shutdown());
            assert_eq!(executor.wake_next_timer(), None, "timer leak");
        }

        #[track_caller]
        fn assert_no_timers(&mut self, executor: &mut fasync::TestExecutor) {
            assert_eq!(executor.run_until_stalled(&mut self.fired.next()), Poll::Pending);
        }

        #[track_caller]
        fn next_timer(&mut self, executor: &mut fasync::TestExecutor) -> TimerId {
            run_in_executor(executor, &mut self.fired.next()).unwrap()
        }
    }

    #[fixture::teardown(TestContext::shutdown_in_executor)]
    #[test]
    fn create_timer_and_fire() {
        set_logger_for_test();
        let mut executor = new_test_executor();
        let mut t = TestContext::new();
        let mut timer_a = t.dispatcher.new_timer(TimerId::A);
        let mut timer_b = t.dispatcher.new_timer(TimerId::B);
        // Never scheduled should never be fired.
        let _timer_c = t.dispatcher.new_timer(TimerId::C);
        t.assert_no_timers(&mut executor);
        // No timers are scheduled.
        assert_eq!(executor.wake_next_timer(), None);
        assert_eq!(timer_a.schedule(T1), None);
        assert_eq!(timer_b.schedule(T3), None);

        executor.set_fake_time(T1);
        assert_eq!(t.next_timer(&mut executor), TimerId::A);

        executor.set_fake_time(T3);
        assert_eq!(t.next_timer(&mut executor), TimerId::B);

        // Reschedule for a time in the past should fire immediately.
        assert_eq!(timer_a.schedule(T2), None);
        assert_eq!(t.next_timer(&mut executor), TimerId::A);

        (t, executor)
    }

    #[fixture::teardown(TestContext::shutdown_in_executor)]
    #[test]
    fn reschedule_for_later() {
        set_logger_for_test();
        let mut executor = new_test_executor();
        let mut t = TestContext::new();
        let mut timer = t.dispatcher.new_timer(TimerId::A);
        assert_eq!(timer.schedule(T1), None);
        assert_eq!(t.heap_len(), 1);
        t.assert_no_timers(&mut executor);
        assert_eq!(timer.schedule(T2).map(Into::into), Some(T1));
        // Reschedule for later doesn't create a new heap entry.
        assert_eq!(t.heap_len(), 1);

        executor.set_fake_time(T1);
        t.assert_no_timers(&mut executor);

        executor.set_fake_time(T2);
        assert_eq!(t.next_timer(&mut executor), TimerId::A);

        (t, executor)
    }

    #[fixture::teardown(TestContext::shutdown_in_executor)]
    #[test]
    fn reschedule_for_earlier() {
        set_logger_for_test();
        let mut executor = new_test_executor();
        let mut t = TestContext::new();
        let mut timer = t.dispatcher.new_timer(TimerId::A);
        assert_eq!(timer.schedule(T2), None);
        assert_eq!(t.heap_len(), 1);
        t.assert_no_timers(&mut executor);
        assert_eq!(timer.schedule(T1).map(Into::into), Some(T2));
        // Rescheduling for earlier creates a new heap entry.
        assert_eq!(t.heap_len(), 2);

        executor.set_fake_time(T1);
        assert_eq!(t.next_timer(&mut executor), TimerId::A);

        executor.set_fake_time(T2);
        t.assert_no_timers(&mut executor);

        (t, executor)
    }

    #[fixture::teardown(TestContext::shutdown_in_executor)]
    #[test]
    fn cancel() {
        set_logger_for_test();
        let mut executor = new_test_executor();
        let mut t = TestContext::new();
        let mut timer = t.dispatcher.new_timer(TimerId::A);

        // Can cancel an unscheduled timer.
        assert_eq!(timer.cancel(), None);

        assert_eq!(timer.schedule(T1), None);
        t.assert_no_timers(&mut executor);
        assert_eq!(timer.cancel().map(Into::into), Some(T1));

        executor.set_fake_time(T1);
        t.assert_no_timers(&mut executor);

        // Can cancel an already canceled timer.
        assert_eq!(timer.cancel(), None);

        // Dropping the timer also cancels it.
        assert_eq!(timer.schedule(T2), None);
        std::mem::drop(timer);
        executor.set_fake_time(T2);
        t.assert_no_timers(&mut executor);

        (t, executor)
    }

    #[fixture::teardown(TestContext::shutdown_in_executor)]
    #[test]
    fn fire_in_order() {
        set_logger_for_test();
        let mut executor = new_test_executor();
        let mut t = TestContext::new();
        let mut timer_a = t.dispatcher.new_timer(TimerId::A);
        let mut timer_b = t.dispatcher.new_timer(TimerId::B);
        let mut timer_c = t.dispatcher.new_timer(TimerId::C);
        assert_eq!(timer_a.schedule(T1), None);
        assert_eq!(timer_b.schedule(T2), None);
        assert_eq!(timer_c.schedule(T3), None);

        executor.set_fake_time(T3);
        assert_eq!(
            executor.run_until_stalled(&mut futures::future::pending::<()>()),
            Poll::Pending
        );

        // All the timers should now be stashed in the receiver.
        assert_eq!(
            t.fired.by_ref().take(3).collect::<Vec<_>>().now_or_never(),
            Some(vec![TimerId::A, TimerId::B, TimerId::C])
        );

        (t, executor)
    }

    #[test]
    fn timer_waiter_starts_terminated() {
        let tw = TimerWaiter::new();
        assert_eq!(tw.is_terminated(), true);
    }

    #[test]
    fn timer_waiter_reuse_timer() {
        set_logger_for_test();
        let mut executor = new_test_executor();
        let mut tw = TimerWaiter::new();
        tw.assert_timers([], None);
        tw.set(T1);
        tw.assert_timers([], Some(T1));
        assert_eq!(tw.is_terminated(), false);
        assert_eq!(executor.wake_next_timer(), Some(T1));
        assert_eq!(executor.run_until_stalled(&mut tw), Poll::Ready(()));
        assert_eq!(tw.is_terminated(), true);
        assert_eq!(executor.wake_next_timer(), None);
        tw.assert_timers([T1], None);
        // Should be able to reuse the timer here.
        tw.set(T2);
        // Initially the timer is still cached to fire at T1, we haven't reset
        // it.
        tw.assert_timers([], Some(T1));
        assert_eq!(executor.wake_next_timer(), None);
        // Then we'll poll once which will update the timer.
        assert_eq!(executor.run_until_stalled(&mut tw), Poll::Pending);
        tw.assert_timers([], Some(T2));
        // Now unblock the future.
        assert_eq!(executor.wake_next_timer(), Some(T2));
        assert_eq!(executor.run_until_stalled(&mut tw), Poll::Ready(()));
        assert_eq!(tw.is_terminated(), true);

        // We can now do everything again, but let's observe the future resolve
        // all at once because time is in the future.
        tw.set(T3);
        tw.assert_timers([], Some(T2));
        executor.set_fake_time(T3);
        assert_eq!(executor.run_until_stalled(&mut tw), Poll::Ready(()));
        assert_eq!(tw.is_terminated(), true);
    }

    #[test]
    fn timer_waiter_cant_reuse() {
        set_logger_for_test();
        let mut executor = new_test_executor();
        let mut tw = TimerWaiter::new();
        tw.set(T3);
        tw.assert_timers([], Some(T3));
        // A time that is earlier than our only available timer will cause us to
        // create a new timer.
        tw.set(T2);
        tw.assert_timers([T3], Some(T2));
        tw.set(T1);
        tw.assert_timers([T2, T3], Some(T1));
        // If we reschedule again to T3, the configuration remains the same
        // because we always prefer our earliest timer.
        tw.set(T3);
        tw.assert_timers([T2, T3], Some(T1));

        // Won't fire until we reach T3.
        executor.set_fake_time(T1);
        assert_eq!(executor.run_until_stalled(&mut tw), Poll::Pending);
        tw.assert_timers([T2, T3], Some(T3));
        executor.set_fake_time(T3);
        assert_eq!(executor.run_until_stalled(&mut tw), Poll::Ready(()));
    }

    #[test]
    fn timer_waiter_target_heap_size() {
        set_logger_for_test();
        let mut executor = new_test_executor();
        let mut tw = TimerWaiter::new();
        tw.target_heap_size = 2;

        // Change the scheduled time in reverse to thrash timer creation.
        tw.set(T4);
        tw.set(T3);
        tw.set(T2);
        tw.set(T1);
        tw.assert_timers([T2, T3, T4], Some(T1));
        executor.set_fake_time(T1);
        assert_eq!(executor.run_until_stalled(&mut tw), Poll::Ready(()));
        // Timer is not reused because we hit the threshold. Note, however, that
        // we're still above threshold because removal is only considered when
        // the timer is observed to have fired.
        tw.assert_timers([T2, T3, T4], None);
    }
}
