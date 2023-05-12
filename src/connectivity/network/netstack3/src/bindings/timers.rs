// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::{
        btree_map,
        hash_map::{Entry, HashMap},
        HashSet,
    },
    fmt::Debug,
    hash::Hash,
    ops::{Deref, DerefMut},
};

use derivative::Derivative;
use fuchsia_async as fasync;
use futures::{
    channel::mpsc,
    future::{Fuse, FutureExt as _},
    stream::StreamExt as _,
};
use tracing::{trace, warn};

use netstack3_core::sync::RwLock as CoreRwLock;

use super::StackTime;

/// The granularity of scheduled timer instants.
///
/// Timers that are requested to be fired at an instant that is not a multiple
/// of the granularity will be rounded up the nearest granularity.
///
/// This is fine because none of the current set of timers expect lower than 1ms
/// granularity:
///
/// Fragmented Packet Reassembly, [RFC 8200 section 4.5]:
///
///   If insufficient fragments are received to complete reassembly of a packet
///   within 60 seconds of the reception of the first-arriving fragment of that
///   packet, reassembly of that packet must be abandoned and all the fragments
///   that have been received for that packet must be discarded.
///
/// Path MTU Discovery, [RFC 8201 section 4]:
///
///   An attempt to detect an increase (by sending a packet larger than the
///   current estimate) must not be done less than 5 minutes after a Packet Too
///   Big message has been received for the given path. The recommended
///   setting for this timer is twice its minimum value (10 minutes).
///
/// NDP Router Advertisement Message Format, [RFC 4861 section 4.2]:
///
///      Router Lifetime
///                     16-bit unsigned integer. The lifetime associated with
///                     the default router in units of seconds. ...
///
///      Reachable Time 32-bit unsigned integer. The time, in milliseconds, that
///                     a node assumes a neighbor is reachable after having
///                     received a reachability confirmation. ...
///
///      Retrans Timer  32-bit unsigned integer. The time, in milliseconds,
///                     between retransmitted Neighbor Solicitation messages.
///                     ...
///
/// NDP Prefix Information, [RFC 4861 section 4.6.2]:
///
///      Valid Lifetime
///                     32-bit unsigned integer. The length of time in seconds
///                     (relative to the time the packet is sent) that the
///                     prefix is valid for the purpose of on-link
///                     determination. ...
///
///      Preferred Lifetime
///                     32-bit unsigned integer. The length of time in seconds
///                     (relative to the time the packet is sent) that addresses
///                     generated from the prefix via stateless address
///                     autoconfiguration remain preferred ...
///
/// NDP Protocol Constants, [RFC 4861 section 10]:
///
///   Router constants:
///
///            MAX_INITIAL_RTR_ADVERT_INTERVAL  16 seconds
///
///            MAX_INITIAL_RTR_ADVERTISEMENTS    3 transmissions
///
///            MAX_FINAL_RTR_ADVERTISEMENTS      3 transmissions
///
///            MIN_DELAY_BETWEEN_RAS             3 seconds
///
///            MAX_RA_DELAY_TIME                 .5 seconds
///
///   Host constants:
///
///            MAX_RTR_SOLICITATION_DELAY        1 second
///
///            RTR_SOLICITATION_INTERVAL         4 seconds
///
///            MAX_RTR_SOLICITATIONS             3 transmissions
///
///   Node constants:
///
///            MAX_MULTICAST_SOLICIT             3 transmissions
///
///            MAX_UNICAST_SOLICIT               3 transmissions
///
///            MAX_ANYCAST_DELAY_TIME            1 second
///
///            MAX_NEIGHBOR_ADVERTISEMENT        3 transmissions
///
///            REACHABLE_TIME               30,000 milliseconds
///
///            RETRANS_TIMER                 1,000 milliseconds
///
///            DELAY_FIRST_PROBE_TIME            5 seconds
///
///            MIN_RANDOM_FACTOR                 .5
///
///            MAX_RANDOM_FACTOR                 1.5
///
/// MLD, [RFC 2710]:
///
///  3.4.  Maximum Response Delay
///
///     The Maximum Response Delay field is meaningful only in Query
///     messages, and specifies the maximum allowed delay before sending a
///     responding Report, in units of milliseconds.
///
///  7.2.  Query Interval
///
///     The Query Interval is the interval between General Queries sent by
///     the Querier.  Default: 125 seconds.
///
///  7.3.  Query Response Interval
///
///     The Maximum Response Delay inserted into the periodic General
///     Queries.  Default: 10000 (10 seconds)
///
///
///  7.8.  Last Listener Query Interval
///
///     The Last Listener Query Interval is the Maximum Response Delay
///     inserted into Multicast-Address-Specific Queries sent in response to
///     Done messages, and is also the amount of time between Multicast-
///     Address-Specific Query messages.  Default: 1000 (1 second)
///
///  7.10.  Unsolicited Report Interval
///
///     The Unsolicited Report Interval is the time between repetitions of a
///     node's initial report of interest in a multicast address.  Default:
///     10 seconds.
///
/// TCP Retransmission Timeout, [RFC 6298 section 2],
///
///   Whenever RTO is computed, if it is less than 1 second, then the RTO SHOULD
///   be rounded up to 1 second.
///
/// Note that although most of the quoted RFCs above are specific to IPv6, IPv4
/// uses roughly the same constants as all of the IPv6 timeouts apply to IPv4 as
/// well.
///
/// [RFC 8200 section 4.5]: https://datatracker.ietf.org/doc/html/rfc8200#section-4.5
/// [RFC 8201 section 4]: https://datatracker.ietf.org/doc/html/rfc8201#section-4
/// [RFC 4861 section 4.2]: https://datatracker.ietf.org/doc/html/rfc4861#section-4.2
/// [RFC 4861 section 4.6.2]: https://datatracker.ietf.org/doc/html/rfc4861#section-4.6.2
/// [RFC 4861 section 10]: https://datatracker.ietf.org/doc/html/rfc4861#section-10
/// [RFC 6298 section 2]: https://datatracker.ietf.org/doc/html/rfc6298#section-2
/// [RFC 2710]: https://datatracker.ietf.org/doc/html/rfc2710
const GRANULARITY: fasync::Duration = fasync::Duration::from_millis(1);

/// Calculates the actual time a timer will fire at according to this
/// timer dispatcher's [`GRANULARITY`].
fn calc_fire_instant(requested_time: fasync::Time) -> fasync::Time {
    // Round the time up to the nearest `GRANULARITY`. This should result in
    // less timers scheduled with the executor and batching of timer firing
    // events.
    //
    // We do not allow rounding down because the stack makes the assumption that
    // a timer will never fire prematurely. Howevever, nothing can reliably know
    // at what actual instant a timer will fire after its scheduled instant. For
    // that reason, we exploit this and intentionally allow timers to fire
    // slightly after the scheduled time but never before. This should result in
    // less wake-ups for the timer dispatcher as well.
    //
    // See http://fxbug.dev/125301 for more details.
    const GRANULARITY_AS_NANOS: i64 = GRANULARITY.into_nanos();
    let granularity_units =
        requested_time.into_nanos().saturating_add(GRANULARITY_AS_NANOS - 1) / GRANULARITY_AS_NANOS;
    // The calculation above doesn't properly handle the case where the instant
    // is close to the maximum possible value because the add above is
    // saturated.
    //
    // Lets say time was backed by `i8` instead of `i64`. `INFINITE` would be
    // `i8::MAX` which is `127`. If we divide that by a granularity of say `10`,
    // we end up with `12` units and `12 * 10 == 120` is less than the value we
    // started at (`127`).
    //
    // Because of this, we know that the multiplication below will never
    // overflow.
    let tentative_fire_instant = fasync::Time::from_nanos(
        GRANULARITY
            .into_nanos()
            .checked_mul(granularity_units)
            .expect("granularity_units can never exeed what is held in a fasync::Time"),
    );
    if tentative_fire_instant >= requested_time {
        tentative_fire_instant
    } else {
        // This means that our requested time is such a large number that we
        // cannot round it up to the next `GRANULARITY` unit. Map all such
        // values to the maximum possible value we can so we still get some
        // amount of batching. This is condition is very unlikely to happen in
        // practice but might as well guard for it.
        fasync::Time::INFINITE
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct InternalId(u64);

impl InternalId {
    fn increment(&self) -> Self {
        let Self(id) = self;
        Self(id.wrapping_add(1))
    }
}

/// Internal information to keep tabs on timers.
struct TimerInfo {
    id: InternalId,
    instant: StackTime,
}

/// A context for specified for a timer type `T` that provides access to a
/// [`TimerHandler`].
pub(crate) trait TimerContext<T: Hash + Eq>: 'static + Clone {
    type Handler: TimerHandler<T>;

    fn handler(&self) -> Self::Handler;
}

/// An entity responsible for receiving expired timers.
///
/// `TimerHandler` is used to communicate expired timers from a
/// [`TimerDispatcher`] that was spawned with some [`TimerContext`].
pub(crate) trait TimerHandler<T: Hash + Eq>: Sized + 'static {
    /// The provided `timer` is expired (its deadline arrived and it wasn't
    /// cancelled or rescheduled).
    fn handle_expired_timer(&mut self, timer: T);
    /// Retrieve a mutable reference to the [`TimerDispatcher`] associated with
    /// this `TimerHandler`. It *must* be the same `TimerDispatcher` instance
    /// for which this handler's [`TimerContext`] was spawned with
    /// [`TimerDispatcher::spawn`].
    fn get_timer_dispatcher(&mut self) -> &TimerDispatcher<T>;
}

#[derive(Debug)]
enum TimerRequestKind {
    Schedule,
    Cancel,
}

#[derive(Debug)]
struct TimerRequest {
    time: StackTime,
    id: InternalId,
    kind: TimerRequestKind,
}

impl TimerRequest {
    fn new(time: StackTime, id: InternalId, kind: TimerRequestKind) -> Self {
        Self { time, id, kind }
    }

    fn new_cancel(time: StackTime, id: InternalId) -> Self {
        Self::new(time, id, TimerRequestKind::Cancel)
    }

    fn new_schedule(time: StackTime, id: InternalId) -> Self {
        Self::new(time, id, TimerRequestKind::Schedule)
    }
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
struct TimerDispatcherInner<T: Hash + Eq> {
    // Invariant: This dispatcher uses a HashMap keyed on an external identifier
    // T and assigns an internal "versioning" ID every time a timer is
    // scheduled. The "versioning" ID is just monotonically incremented and it
    // is used to disambiguate different scheduling events of the same timer T.
    // TimerInfo in the HashMap will always hold the latest allocated
    // "versioning" identifier, meaning that:
    //  - When a timer is rescheduled, we update TimerInfo::id to a new value
    //  - To "commit" a timer firing (through commit_timer), the timer ID
    //    given must carry the same "versioning" identifier as the one currently
    //    held by the HashMap in TimerInfo::id.
    //  - timer_ids will always/only hold the timer IDs that are still scheduled
    //    keyed by their most recent "versioning" ID.
    // The committing mechanism is invisible to external users, which just
    // receive the expired timers through TimerHandler::handle_expired_timer.
    // See TimerDispatcher::spawn for the critical section that makes versioning
    // required.
    timer_ids: HashMap<InternalId, T>,
    timers: HashMap<T, TimerInfo>,
    next_id: InternalId,
    futures_sender: Option<mpsc::UnboundedSender<TimerRequest>>,
}

/// Helper struct to keep track of timers for the event loop.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct TimerDispatcher<T: Hash + Eq> {
    inner: CoreRwLock<TimerDispatcherInner<T>>,
}

impl<T> TimerDispatcher<T>
where
    T: Hash + Debug + Eq + Clone + Send + Sync + Unpin + 'static,
{
    /// Spawns a [`TimerContext`] that will observe events on this
    /// `TimerDispatcher` through its [`TimerHandler`].
    ///
    /// # Panics
    ///
    /// Panics if this `TimerDispatcher` was already spawned.
    pub(crate) fn spawn<C: TimerContext<T> + Send + Sync>(&self, ctx: C) {
        let (sender, mut recv) = mpsc::unbounded();
        {
            let Self { inner } = self;
            let mut inner = inner.write();
            let TimerDispatcherInner { timer_ids: _, timers: _, next_id: _, futures_sender } =
                inner.deref_mut();
            assert!(futures_sender.replace(sender).is_none(), "TimerDispatcher already spawned");
        }

        #[derive(Default)]
        struct TimerGroup {
            ids: HashSet<InternalId>,
            // A timer may be scheduled to fire before a group that already has
            // registered a timer with the executor. In that case, we hold the
            // timer here (to avoid recreating it later) and create a new one
            // for the timer group that is scheduled to fire earlier. This lets
            // us avoid creating redundant timers with the executor.
            fut: Option<Fuse<fasync::Timer>>,
        }

        let mut timers = std::collections::BTreeMap::<fasync::Time, TimerGroup>::default();

        fasync::Task::spawn(async move {
            #[derive(Debug)]
            enum PollResult {
                InstallTimer(TimerRequest),
                TimerFired,
                ReceiverClosed,
            }

            loop {
                // Get the group of timers that should fire next.
                let mut timer_fut = if let Some(entry) = timers.first_entry() {
                    let time = entry.key().clone();
                    let TimerGroup { ids: _, fut } = entry.into_mut();

                    // If we do not yet have a `fasync::Timer` for this group,
                    // create one now. This reduces pressure on the executor's
                    // heap of timers. The executor does not currently handle
                    // aborting timers well (causes a memory leak) so we mitigate
                    // the leak by deferring the creation of timers as much as
                    // possible.
                    //
                    // See http://fxbug.dev/125301 for more details.
                    //
                    // TODO(http://fxbug.dev/125301): Revisit this bandaid for
                    // the executor's memory leak.
                    futures::future::Either::Left(
                        fut.get_or_insert_with(|| fasync::Timer::new(time).fuse()),
                    )
                } else {
                    // No timers scheduled, let this "future" never return.
                    futures::future::Either::Right(futures::future::pending())
                };

                let r = futures::select! {
                    r = recv.next() => match r {
                        Some(req) => PollResult::InstallTimer(req),
                        None => PollResult::ReceiverClosed
                    },
                    () = timer_fut => PollResult::TimerFired,
                };
                // NB: This is the critical section that makes it so that we
                // need to version timers and verify the versioning through
                // `commit_timer` before passing those over to the handler. At
                // this point, the timer future has already resolved. It may
                // already have been aborted, in which case the version ID
                // doesn't matter. But it may also have been already fulfilled.
                // The race comes from the fact that we don't currently have a
                // lock on the context. If we are woken up to handle a timer,
                // the timer ID we're currently holding may have been
                // invalidated by another Task, before this task was woken up so
                // it must NOT be given to to the handler.
                //
                // After this point, we know nothing else can cancel the timer
                // since (as of writing) we use a single-threaded executor and
                // we have no await points below.
                //
                // TODO(https://fxbug.dev/122725): Fix timer dispatcher race
                // with multithreaded executor/concurrent tasks.

                trace!("TimerDispatcher work: {:?}", r);
                match r {
                    PollResult::InstallTimer(TimerRequest { time: StackTime(time), id, kind }) => {
                        let time = calc_fire_instant(time);
                        match kind {
                            TimerRequestKind::Schedule => {
                                assert!(timers.entry(time).or_default().ids.insert(id))
                            }
                            TimerRequestKind::Cancel => {
                                let mut entry = match timers.entry(time) {
                                    btree_map::Entry::Occupied(entry) => entry,
                                    btree_map::Entry::Vacant(_) => continue,
                                };

                                let TimerGroup { ids, fut: _ } = entry.get_mut();

                                // If the group has no more scheduled timers,
                                // throw away the group (including the future).
                                // If the group had a future registered with the
                                // executor (`fasync::Timer`) it is likely that
                                // the executor will clean that up soon since we
                                // lazily create `fasync::Timer`s only for the
                                // next-to-fire group. If the group had no future,
                                // then there are no resources to clean up other
                                // than the memory this entry allocated.
                                let _: bool = ids.remove(&id);
                                if ids.len() == 0 {
                                    let _: (fasync::Time, TimerGroup) = entry.remove_entry();
                                }
                            }
                        }
                    }
                    PollResult::TimerFired => {
                        let mut handler = ctx.handler();

                        // When a timer fires, fire as many timers as we can. Once
                        // we hit a timer group which is expected to fire after
                        // our current time, we go back to awaiting above.
                        while let Some(entry) = timers.first_entry() {
                            let now = fasync::Time::now();
                            if now < entry.key().clone() {
                                break;
                            }

                            let (fire_instant, TimerGroup { ids, fut: _ }) = entry.remove_entry();
                            assert!(
                                now >= fire_instant,
                                "now={now:?}, fire_instant={fire_instant:?}"
                            );

                            for id in ids {
                                match handler.get_timer_dispatcher().commit_timer(id) {
                                    Ok(t) => {
                                        trace!("TimerDispatcher: firing timer {:?}", t);
                                        handler.handle_expired_timer(t);
                                    }
                                    Err(e) => {
                                        trace!("TimerDispatcher: timer was stale {:?}", e);
                                    }
                                }
                            }
                        }
                    }
                    PollResult::ReceiverClosed => break,
                }
            }
        })
        .detach()
    }

    /// Schedule a new timer with identifier `timer_id` at `time`.
    ///
    /// If a timer with the same `timer_id` was already scheduled, the old timer
    /// is unscheduled and its expiry time is returned.
    pub(crate) fn schedule_timer(&self, timer_id: T, time: StackTime) -> Option<StackTime> {
        let Self { inner } = self;
        let mut inner = inner.write();
        let TimerDispatcherInner { timer_ids, timers, next_id, futures_sender } = inner.deref_mut();

        let next_id = {
            let id = *next_id;

            // Overflowing next_id should be safe enough to hold TimerDispatcher's
            // invariant about around "versioning" timer identifiers. We'll
            // overlflow after 2^64 timers are scheduled (which can take a while)
            // and, even then, for it to break the invariant we'd need to still have
            // a timer scheduled from long ago and be unlucky enough that ordering
            // ends up giving it the same ID. That seems unlikely, so we just wrap
            // around and overflow next_id.
            *next_id = next_id.increment();

            id
        };

        if let Some(sender) = futures_sender.as_ref() {
            sender
                .unbounded_send(TimerRequest::new_schedule(time, next_id))
                .expect("TimerDispatcher should not stop");
        } else {
            // Timers are always stopped in tests.
            warn!("TimerDispatcher not spawned, timer {:?} will not fire", timer_id);
        }

        assert_eq!(timer_ids.insert(next_id, timer_id.clone()), None);

        match timers.entry(timer_id) {
            Entry::Vacant(e) => {
                // If we don't have any currently scheduled timers with this
                // timer_id, we're just going to insert a new value into the
                // vacant entry, marking it with next_id.
                let _: &mut TimerInfo = e.insert(TimerInfo { id: next_id, instant: time });
                None
            }
            Entry::Occupied(mut e) => {
                // If we already have a scheduled timer with this timer_id, we
                // must...
                let info = e.get_mut();
                // ...cancel the old timer with the timer dispatcher.
                //
                // Timers are always stopped in tests.
                if let Some(sender) = futures_sender.as_ref() {
                    sender
                        .unbounded_send(TimerRequest::new_cancel(info.instant, info.id))
                        .expect("TimerDispatcher should not stop")
                }
                // ...store the new "versioning" timer_id next_id, effectively
                // marking this newest version as the only valid one, in case
                // the old timer had already fired as is currently waiting to be
                // committed.
                let prev_id = info.id;
                info.id = next_id;
                // ...finally, we get the old instant information to be returned
                // and update the TimerInfo entry with the new time value:
                let old = Some(info.instant);
                info.instant = time;

                assert_eq!(timer_ids.remove(&prev_id).as_ref(), Some(e.key()));

                old
            }
        }
    }

    /// Cancels a timer with identifier `timer_id`.
    ///
    /// If a timer with the provided `timer_id` was scheduled, returns the
    /// expiry time for it after having cancelled it.
    pub(crate) fn cancel_timer(&self, timer_id: &T) -> Option<StackTime> {
        let Self { inner } = self;
        let mut inner = inner.write();
        let TimerDispatcherInner { timer_ids, timers, next_id: _, futures_sender } =
            inner.deref_mut();
        if let Some(t) = timers.remove(timer_id) {
            assert_eq!(timer_ids.remove(&t.id).as_ref(), Some(timer_id));
            // Timers are always stopped in tests.
            if let Some(sender) = futures_sender.as_ref() {
                sender
                    .unbounded_send(TimerRequest::new_cancel(t.instant, t.id))
                    .expect("TimerDispatcher should not stop")
            }
            Some(t.instant)
        } else {
            None
        }
    }

    /// Cancels all timers with given filter.
    ///
    /// `f` will be called sequentially for all the currently scheduled timers.
    /// If `f(id)` returns `true`, the timer with `id` will be cancelled.
    pub(crate) fn cancel_timers_with<F: FnMut(&T) -> bool>(&self, mut f: F) {
        let Self { inner } = self;
        let mut inner = inner.write();
        let TimerDispatcherInner { timer_ids: _, timers, next_id: _, futures_sender } =
            inner.deref_mut();
        timers.retain(|id, info| {
            let discard = f(&id);
            if discard {
                // Timers are always stopped in tests.
                if let Some(sender) = futures_sender.as_ref() {
                    sender
                        .unbounded_send(TimerRequest::new_cancel(info.instant, info.id))
                        .expect("TimerDispatcher should not stop")
                }
            }
            !discard
        });
    }

    /// Gets the time a timer with identifier `timer_id` will be invoked.
    ///
    /// If a timer with the provided `timer_id` exists, returns the expiry
    /// time for it; `None` otherwise.
    pub(crate) fn scheduled_time(&self, timer_id: &T) -> Option<StackTime> {
        let Self { inner } = self;
        let inner = inner.read();
        let TimerDispatcherInner { timer_ids: _, timers, next_id: _, futures_sender: _ } =
            inner.deref();
        timers.get(timer_id).map(|t| t.instant)
    }

    /// Retrieves the internal timer value of a timer ID.
    ///
    /// `commit_timer` will "commit" `id` for consumption, if `id` is
    /// still valid to be triggered. If `commit_timer` returns `Ok`, then
    /// `TimerDispatcher` will "forget" about the timer identifier referenced
    /// by `id`, meaning subsequent calls to [`cancel_timer`] or
    /// [`schedule_timer`] will return `Err`.
    ///
    /// [`cancel_timer`]: TimerDispatcher::cancel_timer
    /// [`schedule_timer`]: TimerDispatcher::schedule_timer
    fn commit_timer(&self, id: InternalId) -> Result<T, InternalId> {
        let Self { inner } = self;
        let mut inner = inner.write();
        let TimerDispatcherInner { timer_ids, timers, next_id: _, futures_sender: _ } =
            inner.deref_mut();
        let external_id = timer_ids.remove(&id).ok_or(id)?;
        let info = timers.remove(&external_id).ok_or(id)?;
        assert_eq!(info.id, id);
        Ok(external_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::integration_tests::set_logger_for_test;
    use assert_matches::assert_matches;
    use futures::{channel::mpsc, task::Poll, Future, StreamExt};
    use std::sync::Arc;
    use test_case::test_case;

    type TestDispatcher = TimerDispatcher<usize>;

    #[derive(Clone)]
    struct TestContext {
        dispatcher: Arc<TestDispatcher>,
        fired: mpsc::UnboundedSender<usize>,
    }

    impl TimerHandler<usize> for TestContext {
        fn handle_expired_timer(&mut self, timer: usize) {
            self.fired.unbounded_send(timer).expect("Can't fire timer")
        }
        fn get_timer_dispatcher(&mut self) -> &TimerDispatcher<usize> {
            &self.dispatcher
        }
    }

    impl TestContext {
        fn new() -> (Self, mpsc::UnboundedReceiver<usize>) {
            let (fired, receiver) = mpsc::unbounded();
            let this = TestContext { dispatcher: TestDispatcher::default().into(), fired };
            this.dispatcher.spawn(this.clone());
            (this, receiver)
        }

        fn with_disp_sync<R, F: FnOnce(&TestDispatcher) -> R>(&self, f: F) -> R {
            f(&self.dispatcher)
        }
    }

    impl TimerContext<usize> for TestContext {
        type Handler = Self;
        fn handler(&self) -> Self {
            self.clone()
        }
    }

    // Returns a new test executor initialized with an initial time set to 0.
    fn new_test_executor() -> fasync::TestExecutor {
        // Set to zero to simplify logs in tests.
        new_test_executor_with_initial_time(fasync::Time::from_nanos(0))
    }

    fn new_test_executor_with_initial_time(time: fasync::Time) -> fasync::TestExecutor {
        let executor = fasync::TestExecutor::new_with_fake_time();
        // Set to zero to simplify logs in tests.
        executor.set_fake_time(time);
        executor
    }

    fn time_from_now(units: i64) -> StackTime {
        StackTime(fasync::Time::after(GRANULARITY * units))
    }

    fn run_in_executor<R, Fut: Future<Output = R>>(
        executor: &mut fasync::TestExecutor,
        f: Fut,
    ) -> R {
        futures::pin_mut!(f);
        loop {
            executor.wake_main_future();
            match executor.run_one_step(&mut f) {
                Some(Poll::Ready(x)) => break x,
                None => panic!("Executor stalled"),
                Some(Poll::Pending) => (),
            }
        }
    }

    #[test_case(
        fasync::Time::from_nanos(0),
        fasync::Time::from_nanos(0)
    ; "zero")]
    #[test_case(
        fasync::Time::from_nanos(1),
        fasync::Time::from_nanos(GRANULARITY.into_nanos())
    ; "smallest non-zero")]
    #[test_case(
        fasync::Time::from_nanos(GRANULARITY.into_nanos() - 1),
        fasync::Time::from_nanos(GRANULARITY.into_nanos())
    ; "slightly less than granularity")]
    #[test_case(
        fasync::Time::from_nanos(GRANULARITY.into_nanos()),
        fasync::Time::from_nanos(GRANULARITY.into_nanos())
    ; "exactly granularity")]
    #[test_case(
        fasync::Time::from_nanos(GRANULARITY.into_nanos() + 1),
        fasync::Time::from_nanos(GRANULARITY.into_nanos() * 2)
    ; "slightly more than granularity")]
    #[test_case(
        fasync::Time::INFINITE - fasync::Duration::from_nanos(1),
        fasync::Time::INFINITE
    ; "slightly less than infinite")]
    #[test_case(
        fasync::Time::INFINITE,
        fasync::Time::INFINITE
    ; "infinite")]
    fn calc_fire_instant(req_time: fasync::Time, want_time: fasync::Time) {
        assert_eq!(super::calc_fire_instant(req_time), want_time);
    }

    #[test_case(fasync::Time::from_nanos(0); "zero")]
    #[test_case(fasync::Time::from_nanos(GRANULARITY.into_nanos()); "non-zero")]
    fn test_timers_fire(initial_time: fasync::Time) {
        set_logger_for_test();
        let mut executor = new_test_executor_with_initial_time(initial_time);

        const NANOSECOND: fasync::Duration = fasync::Duration::from_nanos(1);

        let batches = [
            [
                StackTime(fasync::Time::after(NANOSECOND)),
                StackTime(fasync::Time::after(NANOSECOND * 2)),
                StackTime(fasync::Time::after(GRANULARITY)),
            ],
            [
                StackTime(fasync::Time::after(GRANULARITY + NANOSECOND)),
                StackTime(fasync::Time::after(GRANULARITY + NANOSECOND * 2)),
                StackTime(fasync::Time::after(GRANULARITY + NANOSECOND * 2)),
            ],
        ];

        let (t, mut fired) = TestContext::new();
        run_in_executor(&mut executor, async {
            let d = t.handler();

            let mut id = 1;
            for batch in batches {
                for instant in batch {
                    assert_eq!(d.dispatcher.schedule_timer(id, instant), None);
                    id += 1;
                }
            }
        });

        let mut first_id = 1;
        for batch in batches {
            assert_matches!(fired.try_next(), Err(mpsc::TryRecvError { .. }));
            executor.set_fake_time(fasync::Time::after(GRANULARITY));

            let fired_ids = batch
                .into_iter()
                .map(|_: StackTime| run_in_executor(&mut executor, fired.next()).unwrap())
                .collect::<HashSet<_>>();
            let next_first_id = first_id + batch.len();
            assert_eq!(fired_ids, HashSet::from_iter(first_id..next_first_id),);

            first_id = next_first_id;
        }
    }

    #[test]
    fn test_get_scheduled_instant() {
        set_logger_for_test();
        let mut _executor = new_test_executor();
        let (t, _) = TestContext::new();

        let d = &t.dispatcher;

        // Timer 1 is scheduled.
        let time1 = time_from_now(1);
        assert_eq!(d.schedule_timer(1, time1), None);
        assert_eq!(d.scheduled_time(&1).unwrap(), time1);

        // Timer 2 does not exist yet.
        assert_eq!(d.scheduled_time(&2), None);

        // Timer 1 is scheduled.
        let time2 = time_from_now(2);
        assert_eq!(d.schedule_timer(2, time2), None);
        assert_eq!(d.scheduled_time(&1).unwrap(), time1);
        assert_eq!(d.scheduled_time(&2).unwrap(), time2);

        // Cancel Timer 1.
        assert_eq!(d.cancel_timer(&1).unwrap(), time1);
        assert_eq!(d.scheduled_time(&1), None);

        // Timer 2 should still be scheduled.
        assert_eq!(d.scheduled_time(&2).unwrap(), time2);
    }

    #[test]
    fn test_cancel() {
        set_logger_for_test();
        let mut executor = new_test_executor();
        let (t, mut rcv) = TestContext::new();

        // timer 1 and 2 are scheduled.
        // timer 1 is going to be cancelled even before we allow the loop to
        // run.

        let time1 = time_from_now(1);
        let time2 = time_from_now(2);
        let time3 = time_from_now(5);
        t.with_disp_sync(|d| {
            assert_eq!(d.schedule_timer(1, time1), None);
            assert_eq!(d.schedule_timer(2, time2), None);

            assert_eq!(d.cancel_timer(&1).unwrap(), time1);
        });
        executor.set_fake_time(time2.0);
        let r = run_in_executor(&mut executor, rcv.next()).unwrap();
        t.with_disp_sync(|d| {
            // can't cancel 2 anymore, it has already fired
            assert_eq!(d.cancel_timer(&2), None);
        });
        // only event 2 should come out because 1 was cancelled:
        assert_eq!(r, 2);

        // schedule another timer and wait for it, just to prove that timer 1's
        // event never gets fired:
        t.with_disp_sync(|d| {
            assert_eq!(d.schedule_timer(3, time3), None);
        });
        executor.set_fake_time(time3.0);
        let r = run_in_executor(&mut executor, rcv.next()).unwrap();
        assert_eq!(r, 3);
    }

    #[test]
    fn test_reschedule() {
        set_logger_for_test();
        let mut executor = new_test_executor();
        let (t, mut rcv) = TestContext::new();

        // timer 1 and 2 are scheduled.
        // timer 1 is going to be rescheduled even before we allow the loop to
        // run.
        let time1 = time_from_now(1);
        let time2 = time_from_now(2);
        let resched1 = time_from_now(3);
        let resched2 = time_from_now(4);

        t.with_disp_sync(|d| {
            assert_eq!(d.schedule_timer(1, time1), None);
            assert_eq!(d.schedule_timer(2, time2), None);
            assert_eq!(d.schedule_timer(1, resched1).unwrap(), time1);
        });
        executor.set_fake_time(time2.0);
        let r = run_in_executor(&mut executor, rcv.next()).unwrap();
        // only event 2 should come out:
        assert_eq!(r, 2);

        t.with_disp_sync(|d| {
            // we can schedule timer 2 again, and it returns None because it has
            // already fired.
            assert_eq!(d.schedule_timer(2, resched2), None);
        });

        // now we can go at it again and get the rescheduled timers:
        executor.set_fake_time(resched2.0);
        assert_eq!(run_in_executor(&mut executor, rcv.next()).unwrap(), 1);
        assert_eq!(run_in_executor(&mut executor, rcv.next()).unwrap(), 2);
    }

    #[test]
    fn test_cancel_with() {
        set_logger_for_test();
        let mut executor = new_test_executor();
        let (t, mut rcv) = TestContext::new();

        t.with_disp_sync(|d| {
            // schedule 4 timers:
            assert_eq!(d.schedule_timer(1, time_from_now(1)), None);
            assert_eq!(d.schedule_timer(2, time_from_now(2)), None);
            assert_eq!(d.schedule_timer(3, time_from_now(3)), None);
            assert_eq!(d.schedule_timer(4, time_from_now(4)), None);

            // cancel timers 1, 3, and 4.
            d.cancel_timers_with(|id| *id != 2);
            // check that only one timer remains
            assert_eq!(d.inner.read().timers.len(), 1);
        });
        // advance time so that all timers would've been fired.
        executor.set_fake_time(time_from_now(4).0);
        // get the timer and assert that it is the timer with id == 2.
        let r = run_in_executor(&mut executor, rcv.next()).unwrap();
        assert_eq!(r, 2);
    }
}
