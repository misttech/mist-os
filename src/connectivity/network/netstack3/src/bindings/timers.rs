// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    cmp,
    collections::BinaryHeap,
    fmt::Debug,
    sync::{
        atomic::{self, AtomicI64, AtomicUsize},
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
    inner: Arc<TimerDispatcherInner<T>>,
}

impl<T> Default for TimerDispatcher<T> {
    fn default() -> Self {
        let notifier = NeedsDataNotifier::default();
        let watcher = notifier.watcher();
        Self {
            inner: Arc::new(TimerDispatcherInner {
                timer_count: AtomicUsize::new(0),
                state: CoreMutex::new(TimerDispatcherState {
                    heap: BinaryHeap::new(),
                    notifier: Some(notifier),
                    watcher: Some(watcher),
                }),
            }),
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
        let watcher =
            self.inner.state.lock().watcher.take().expect("timer dispatcher already spawned");
        fasync::Task::spawn(Self::worker(handler, watcher, Arc::clone(&self.inner)))
    }

    /// Creates a new timer with identifier `dispatch` on this `dispatcher`.
    pub(crate) fn new_timer(&self, dispatch: T) -> Timer<T> {
        let _: usize = self.inner.timer_count.fetch_add(1, atomic::Ordering::SeqCst);
        Timer {
            heap: Arc::clone(&self.inner),
            state: Arc::new(TimerState {
                dispatch,
                scheduled: AtomicI64::new(UNSCHEDULED_SENTINEL.into_nanos()),
                gc_generation: AtomicUsize::new(0),
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
        assert!(self.inner.state.lock().notifier.take().is_some(), "dispatcher already closed");
    }

    async fn worker<H: FnMut(T)>(
        mut handler: H,
        mut watcher: NeedsDataWatcher,
        inner: Arc<TimerDispatcherInner<T>>,
    ) {
        let mut timer = TimerWaiter::new();
        let mut gc_generation = 0;
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

            let (next_wakeup, heap_len) = Self::check_timer_heap(&mut handler, &inner.state).await;

            trace!("next wakeup = {:?}, heap_len = {heap_len}", ScheduledInstant::new(next_wakeup));
            // Update our timer to wake at the next wake up time according to
            // the heap.
            timer.set(next_wakeup);

            // Our target heap size is the number of timers we have alive,
            // capped to a minimum size to avoid too much GC thrash.
            //
            // We'll run a GC on the heap whenever we hit twice the target size,
            // at which point we expect the GC to bring the number of entries
            // down to around the target size.
            let target_heap_len =
                inner.timer_count.load(atomic::Ordering::SeqCst).max(MIN_TARGET_HEAP_SIZE);
            timer.set_target_heap_size(target_heap_len);
            if heap_len > target_heap_len * 2 {
                gc_generation += 1;
                let after = Self::gc_timer_heap(&inner.state, gc_generation);
                trace!(
                    "timer gc gen({}) triggered with heap_len={}, target={}, after={}",
                    gc_generation,
                    heap_len,
                    target_heap_len,
                    after
                );
            }
        }
        // Cleanup everything on the heap before returning.
        inner.state.lock().heap.clear();
    }

    /// Walks the timer heap, removing any stale entries that might have
    /// accumulated due to timer scheduling/cancelation thrash.
    ///
    /// Returns the heap length after GC.
    fn gc_timer_heap(inner: &CoreMutex<TimerDispatcherState<T>>, generation: usize) -> usize {
        let mut guard = inner.lock();
        let TimerDispatcherState { heap, notifier: _, watcher: _ } = &mut *guard;
        heap.retain(|TimeAndValue { time, value }| {
            let current = fasync::Time::from_nanos(value.scheduled.load(atomic::Ordering::SeqCst));
            // Retain all the entries that are still valid.
            match ScheduledEntryValidity::new(current, *time) {
                ScheduledEntryValidity::Valid | ScheduledEntryValidity::ValidForLaterTime => {
                    // Only keep entries that we haven't seen yet on this GC
                    // sweep. It is possible to observe many `ValidForLaterTime`
                    // entries for a single timer and we want to keep only one
                    // of them.
                    value.gc_generation.swap(generation, atomic::Ordering::SeqCst) != generation
                }
                ScheduledEntryValidity::Invalid => false,
            }
        });
        heap.len()
    }

    /// Checks the timer heap, firing any elapsed timers.
    ///
    /// Returns the next wakeup time for the worker task and the total number of
    /// entries in the internal heap.
    async fn check_timer_heap<H: FnMut(T)>(
        handler: &mut H,
        inner: &CoreMutex<TimerDispatcherState<T>>,
    ) -> (fasync::Time, usize) {
        let mut fired_count = 0;
        loop {
            let fire = {
                let mut guard = inner.lock();
                let TimerDispatcherState { heap, notifier: _, watcher: _ } = &mut *guard;
                let heap_len = heap.len();
                let Some(front) = heap.peek_mut() else {
                    // Nothing to wait for.
                    return (UNSCHEDULED_SENTINEL, heap_len);
                };
                if !front.should_fire_at(fasync::Time::now()) {
                    // Wait until the time at the front of the heap to fire.
                    return (front.time, heap_len);
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

struct TimerDispatcherState<T> {
    heap: BinaryHeap<TimerScheduledEntry<T>>,
    // Notifier is removed on close.
    notifier: Option<NeedsDataNotifier>,
    // Watcher is taken on spawn.
    watcher: Option<NeedsDataWatcher>,
}

struct TimerDispatcherInner<T> {
    timer_count: AtomicUsize,
    state: CoreMutex<TimerDispatcherState<T>>,
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

/// The validity state of a [`TimerScheduledEntry`].
enum ScheduledEntryValidity {
    /// This entry is valid and can cause a timer to fire.
    Valid,
    /// This entry is valid at a later time, loaded from the timer state.
    ValidForLaterTime,
    /// This entry is invalid and can be discarded.
    Invalid,
}

impl ScheduledEntryValidity {
    /// Evaluates the validity of a scheduled entry with `current_value` and
    /// cached `scheduled_entry`.
    fn new(current_value: fasync::Time, scheduled_entry: fasync::Time) -> Self {
        if current_value == UNSCHEDULED_SENTINEL {
            return Self::Invalid;
        }
        match current_value.cmp(&scheduled_entry) {
            // If the timer state is holding a time before the one originally
            // scheduled, this is a stale timer entry. We should ignore it.
            cmp::Ordering::Less => Self::Invalid,
            cmp::Ordering::Equal => Self::Valid,
            // If the timer state is holding a time later than the originally
            // scheduled, it needs to be replaced in the heap to be fired later.
            cmp::Ordering::Greater => Self::ValidForLaterTime,
        }
    }
}

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
                match ScheduledEntryValidity::new(next, scheduled_for) {
                    // If the timer state is  valid for a later time, we need to
                    // put it back into the heap to be fired later.
                    ScheduledEntryValidity::ValidForLaterTime => {
                        TryFireResult::Reschedule(Self { value: timer_state, time: next })
                    }
                    // Ignore stale and invalid entries.
                    ScheduledEntryValidity::Invalid => TryFireResult::Ignore,
                    // The valid case, when the current and scheduled_for times
                    // are the same is covered by the Ok arm above.
                    ScheduledEntryValidity::Valid => unreachable!(),
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
    // State kept with each timer to help GC.
    gc_generation: AtomicUsize,
}

/// This type is preventing [`Timer`] from evolving poorly and deriving `Clone`.
struct NoCloneGuard;

pub(crate) struct Timer<T> {
    state: Arc<TimerState<T>>,
    heap: Arc<TimerDispatcherInner<T>>,
    /// This type must not become Clone, see explanation in [`Timer::schedule`].
    _no_clone: NoCloneGuard,
}

impl<T> Debug for Timer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Many structs in core that use timers are self-referential so we need
        // to be careful in the Debug impl here, especially around the opaque
        // type T.
        let Self { state, heap: _, _no_clone: _ } = self;
        let TimerState { scheduled, dispatch: _, gc_generation: _ } = &**state;
        let scheduled = ScheduledInstant::new(fasync::Time::from_nanos(
            scheduled.load(atomic::Ordering::Relaxed),
        ));
        f.debug_struct("Timer").field("scheduled", &scheduled).finish_non_exhaustive()
    }
}

impl<T> Drop for Timer<T> {
    fn drop(&mut self) {
        let _: Option<ScheduledInstant> = self.cancel();
        let _: usize = self.heap.timer_count.fetch_sub(1, atomic::Ordering::SeqCst);
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
            let mut guard = self.heap.state.lock();
            let wake_notifier = {
                let TimerDispatcherState { heap, notifier, watcher: _ } = &mut *guard;
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

const MIN_TARGET_HEAP_SIZE: usize = 10;

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
            target_heap_size: MIN_TARGET_HEAP_SIZE,
        }
    }

    /// Sets the target heap size to `target`.
    fn set_target_heap_size(&mut self, target: usize) {
        self.target_heap_size = target;
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
    use std::pin::pin;

    use crate::bindings::integration_tests::set_logger_for_test;

    use fuchsia_zircon as zx;
    use futures::channel::mpsc;
    use test_case::test_case;

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
        let mut f = pin!(f);
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
        Other(usize),
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
            self.dispatcher.inner.state.lock().heap.len()
        }

        fn schedule_notifier(&self) {
            self.dispatcher.inner.state.lock().notifier.as_ref().unwrap().schedule()
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

    #[fixture::teardown(TestContext::shutdown_in_executor)]
    #[test_case(1; "min_target_size")]
    #[test_case(MIN_TARGET_HEAP_SIZE * 2; "timer_count")]
    fn heap_gc(timer_count: usize) {
        set_logger_for_test();
        let mut executor = new_test_executor();
        let mut t = TestContext::new();
        assert_eq!(t.dispatcher.inner.timer_count.load(atomic::Ordering::SeqCst), 0);
        let mut timers =
            (0..timer_count).map(|i| t.dispatcher.new_timer(TimerId::Other(i))).collect::<Vec<_>>();
        let timer = &mut timers[0];
        assert_eq!(t.dispatcher.inner.timer_count.load(atomic::Ordering::SeqCst), timer_count);

        assert_eq!(timer.schedule(T1), None);
        assert_eq!(t.heap_len(), 1);

        let mut reschedule = || {
            let prev: fasync::Time = timer.cancel().unwrap().into();
            assert_eq!(timer.schedule(prev + zx::Duration::from_seconds(1)), None);
        };

        // Canceling and rescheduling creates a new heap entry.
        reschedule();
        assert_eq!(t.heap_len(), 2);
        // GC won't be triggered until we hit the gc threshold.
        t.assert_no_timers(&mut executor);
        assert_eq!(t.heap_len(), 2);
        let threshold = timer_count.max(MIN_TARGET_HEAP_SIZE) * 2;
        while t.heap_len() < threshold {
            reschedule();
        }

        // Kick the notifier and check to check that GC doesn't run yet.
        t.schedule_notifier();
        t.assert_no_timers(&mut executor);
        assert_eq!(t.heap_len(), threshold);

        // Do one more round to push over the threshold and check that GC runs.
        reschedule();
        t.schedule_notifier();
        t.assert_no_timers(&mut executor);
        assert_eq!(t.heap_len(), 1);

        // On drop the number of timers should decrease.
        drop(timers);
        assert_eq!(t.dispatcher.inner.timer_count.load(atomic::Ordering::SeqCst), 0);

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
