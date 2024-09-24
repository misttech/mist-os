// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Support for creating futures that represent timers.
//!
//! This module contains the `Timer` type which is a future that will resolve
//! at a particular point in the future.

use crate::runtime::{EHandle, Time, WakeupTime};
use crate::PacketReceiver;
use fuchsia_sync::Mutex;
use fuchsia_zircon as zx;
use fuchsia_zircon::AsHandleRef as _;
use futures::stream::FusedStream;
use futures::task::{AtomicWaker, Context};
use futures::{FutureExt, Stream};
use std::collections::BinaryHeap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::task::Poll;
use std::{cmp, fmt};

pub trait TimeInterface:
    Clone + Copy + fmt::Debug + PartialEq + PartialOrd + Ord + Send + Sync + 'static
{
    type Timeline: zx::Timeline + Send + Sync + 'static;

    fn into_zx(self) -> zx::Time<Self::Timeline>;
    fn now() -> Self;
}

impl TimeInterface for Time {
    type Timeline = zx::MonotonicTimeline;

    fn into_zx(self) -> zx::MonotonicTime {
        self.into_zx()
    }

    fn now() -> Self {
        EHandle::local().inner().now()
    }
}

impl WakeupTime for std::time::Instant {
    fn into_time(self) -> Time {
        let now_as_instant = std::time::Instant::now();
        let now_as_time = Time::now();
        now_as_time + self.saturating_duration_since(now_as_instant).into()
    }
}

impl WakeupTime for Time {
    fn into_time(self) -> Time {
        self
    }
}

impl WakeupTime for zx::MonotonicTime {
    fn into_time(self) -> Time {
        self.into()
    }
}

/// An asynchronous timer.
#[derive(Debug)]
#[must_use = "futures do nothing unless polled"]
pub struct Timer {
    waker_and_bool: Arc<(AtomicWaker, AtomicBool)>,
}

impl Unpin for Timer {}

impl Timer {
    /// Create a new timer scheduled to fire at `time`.
    pub fn new<WT>(time: WT) -> Self
    where
        WT: WakeupTime,
    {
        let waker_and_bool = Arc::new((AtomicWaker::new(), AtomicBool::new(false)));
        let this = Timer { waker_and_bool };
        EHandle::local().register_timer(time.into_time(), this.handle());
        this
    }

    fn handle(&self) -> TimerHandle {
        TimerHandle { inner: Arc::downgrade(&self.waker_and_bool) }
    }

    /// Reset the `Timer` to a fire at a new time.
    /// The `Timer` must have already fired since last being reset.
    pub fn reset(&mut self, time: Time) {
        assert!(self.did_fire());
        self.waker_and_bool.1.store(false, Ordering::SeqCst);
        EHandle::local().register_timer(time, self.handle());
    }

    fn did_fire(&self) -> bool {
        self.waker_and_bool.1.load(Ordering::SeqCst)
    }

    fn register_task(&self, cx: &mut Context<'_>) {
        self.waker_and_bool.0.register(cx.waker());
    }
}

impl Future for Timer {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // See https://docs.rs/futures/0.3.5/futures/task/struct.AtomicWaker.html
        // for more information.
        // quick check to avoid registration if already done.
        if self.did_fire() {
            return Poll::Ready(());
        }

        self.register_task(cx);

        // Need to check condition **after** `register` to avoid a race
        // condition that would result in lost notifications.
        if self.did_fire() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

pub(crate) struct TimerHandle {
    inner: Weak<(AtomicWaker, AtomicBool)>,
}

impl TimerHandle {
    fn is_defunct(&self) -> bool {
        self.inner.upgrade().is_none()
    }

    pub fn wake(&self) {
        if let Some(wb) = self.inner.upgrade() {
            wb.1.store(true, Ordering::SeqCst);
            wb.0.wake();
        }
    }
}

/// An asynchronous interval timer.
/// This is a stream of events resolving at a rate of once-per interval.
#[derive(Debug)]
#[must_use = "streams do nothing unless polled"]
pub struct Interval {
    timer: Timer,
    next: Time,
    duration: zx::Duration,
}

impl Interval {
    /// Create a new `Interval` which yields every `duration`.
    pub fn new(duration: zx::Duration) -> Self {
        let next = Time::after(duration);
        Interval { timer: Timer::new(next), next, duration }
    }
}

impl Unpin for Interval {}

impl FusedStream for Interval {
    fn is_terminated(&self) -> bool {
        // `Interval` never yields `None`
        false
    }
}

impl Stream for Interval {
    type Item = ();
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        match this.timer.poll_unpin(cx) {
            Poll::Ready(()) => {
                this.timer.register_task(cx);
                this.next += this.duration;
                this.timer.reset(this.next);
                Poll::Ready(Some(()))
            }
            Poll::Pending => {
                this.timer.register_task(cx);
                Poll::Pending
            }
        }
    }
}

pub(crate) struct TimerHeap<T: TimeInterface> {
    inner: Mutex<Inner<T>>,
}

struct Inner<T: TimeInterface> {
    // We can't easily use ReceiverRegistration because there would be a circular dependency we'd
    // have to break: Executor -> TimerHeap -> ReceiverRegistration -> EHandle -> ScopeRef ->
    // Executor, so instead we just store the port key and then change the executor to drop the
    // registration at the appropriate place.
    port_key: u64,

    fake: bool,

    heap: BinaryHeap<TimeWaker<T>>,

    // True if there's a pending async_wait.
    async_wait: bool,

    timer: zx::Timer<T::Timeline>,
}

impl<T: TimeInterface> Inner<T> {
    fn set_timer(&mut self, time: T) {
        self.timer.set(time.into_zx(), Default::default()).unwrap();

        if !self.async_wait {
            if self.fake {
                // Clear the signal used for fake timers so that we can use it to trigger
                // next time.
                self.timer.signal_handle(zx::Signals::USER_0, zx::Signals::empty()).unwrap();
            }

            self.timer
                .wait_async_handle(
                    EHandle::local().port(),
                    self.port_key,
                    if self.fake { zx::Signals::USER_0 } else { zx::Signals::TIMER_SIGNALED },
                    zx::WaitAsyncOpts::empty(),
                )
                .unwrap();

            self.async_wait = true;
        }
    }
}

impl TimerHeap<Time> {
    pub fn new(port_key: u64, fake: bool) -> Self {
        Self {
            inner: Mutex::new(Inner {
                port_key,
                fake,
                heap: BinaryHeap::default(),
                async_wait: false,
                timer: zx::Timer::<zx::MonotonicTimeline>::create(),
            }),
        }
    }
}

impl<T: TimeInterface> TimerHeap<T> {
    pub fn port_key(&self) -> u64 {
        self.inner.lock().port_key
    }

    /// Adds a timer.
    pub fn add_timer(&self, time: T, handle: TimerHandle) {
        let mut inner = self.inner.lock();
        if T::now() >= time {
            handle.wake();
            return;
        }
        if inner.heap.peek().map_or(true, |t| time < t.time) {
            inner.set_timer(time);
        }
        inner.heap.push(TimeWaker { time, handle });
    }

    /// Wakes timers that should be firing now.  Returns true if any timers were woken.
    pub fn wake_timers(&self) -> bool {
        self.wake_timers_impl(false)
    }

    fn wake_timers_impl(&self, from_receive_packet: bool) -> bool {
        let now = T::now();
        let mut timers_woken = false;
        loop {
            let timer = {
                let mut inner = self.inner.lock();

                if from_receive_packet {
                    inner.async_wait = false;
                }

                while inner.heap.peek().map_or(false, |t| t.handle.is_defunct()) {
                    inner.heap.pop();
                }

                match inner.heap.peek() {
                    Some(t) => {
                        if t.time <= now {
                            inner.heap.pop().unwrap()
                        } else {
                            let time = t.time;
                            inner.set_timer(time);
                            break;
                        }
                    }
                    _ => break,
                }
            };
            timer.handle.wake();
            timers_woken = true;
        }
        timers_woken
    }

    /// Wakes the next timer and returns its time.
    pub fn wake_next_timer(&self) -> Option<T> {
        let mut inner = self.inner.lock();
        loop {
            let timer = inner.heap.pop();
            if let Some(timer) = timer {
                if timer.handle.is_defunct() {
                    continue;
                }
                std::mem::drop(inner);
                timer.handle.wake();
                return Some(timer.time);
            } else {
                return None;
            }
        }
    }

    /// Returns the next timer due to expire.
    pub fn next_timer(&self) -> Option<T> {
        let mut inner = self.inner.lock();
        loop {
            if let Some(timer) = inner.heap.peek() {
                if timer.handle.is_defunct() {
                    inner.heap.pop();
                    continue;
                }
                return Some(timer.time);
            } else {
                return None;
            }
        }
    }

    /// If there's a timer ready, sends a notification to wake up the receiver.
    ///
    /// # Panics
    ///
    /// This will panic if we are not using fake time.
    pub fn maybe_notify(&self, now: T) {
        let inner = self.inner.lock();
        assert!(inner.fake);
        if inner.heap.peek().map_or(false, |t| t.time <= now) {
            inner.timer.signal_handle(zx::Signals::empty(), zx::Signals::USER_0).unwrap();
        }
    }
}

impl<T: TimeInterface> PacketReceiver for TimerHeap<T> {
    fn receive_packet(&self, _packet: zx::Packet) {
        self.wake_timers_impl(true);
    }
}

struct TimeWaker<T> {
    time: T,
    handle: TimerHandle,
}

impl<T: TimeInterface> Ord for TimeWaker<T> {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.time.cmp(&other.time).reverse() // Reverse to get min-heap rather than max
    }
}

impl<T: TimeInterface> PartialOrd for TimeWaker<T> {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: TimeInterface> PartialEq for TimeWaker<T> {
    /// BinaryHeap requires `TimeWaker: Ord` above so that there's a total ordering between
    /// elements, and `T: Ord` requires `T: Eq` even we don't actually need to check these for
    /// equality. We could use `Weak::ptr_eq` to check the handles here, but then that would cause
    /// the `PartialEq` implementation to return false in some cases where `Ord` returns
    /// `Ordering::Equal`, which is asking for logic errors down the line.
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time
    }
}
impl<T: TimeInterface> Eq for TimeWaker<T> {}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{LocalExecutor, SendExecutor, TestExecutor};
    use assert_matches::assert_matches;
    use fuchsia_zircon::Duration;
    use futures::future::Either;
    use futures::prelude::*;

    #[test]
    fn shorter_fires_first() {
        let mut exec = LocalExecutor::new();
        let shorter = Timer::new(Time::after(Duration::from_millis(100)));
        let longer = Timer::new(Time::after(Duration::from_seconds(1)));
        match exec.run_singlethreaded(future::select(shorter, longer)) {
            Either::Left(_) => {}
            Either::Right(_) => panic!("wrong timer fired"),
        }
    }

    #[test]
    fn shorter_fires_first_multithreaded() {
        let mut exec = SendExecutor::new(4);
        let shorter = Timer::new(Time::after(Duration::from_millis(100)));
        let longer = Timer::new(Time::after(Duration::from_seconds(1)));
        match exec.run(future::select(shorter, longer)) {
            Either::Left(_) => {}
            Either::Right(_) => panic!("wrong timer fired"),
        }
    }

    #[test]
    fn fires_after_timeout() {
        let mut exec = TestExecutor::new_with_fake_time();
        exec.set_fake_time(Time::from_nanos(0));
        let deadline = Time::after(Duration::from_seconds(5));
        let mut future = Timer::new(deadline);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut future));
        exec.set_fake_time(deadline);
        assert_eq!(Poll::Ready(()), exec.run_until_stalled(&mut future));
    }

    #[test]
    fn timer_before_now_fires_immediately() {
        let mut exec = TestExecutor::new();
        let now = Time::now();
        let before = Timer::new(now - Duration::from_nanos(1));
        let after = Timer::new(now + Duration::from_nanos(1));
        assert_matches!(
            exec.run_singlethreaded(futures::future::select(before, after)),
            Either::Left(_),
            "Timer in the past should fire first"
        );
    }

    #[test]
    fn interval() {
        let mut exec = TestExecutor::new_with_fake_time();
        let start = Time::from_nanos(0);
        exec.set_fake_time(start);

        let counter = Arc::new(::std::sync::atomic::AtomicUsize::new(0));
        let mut future = {
            let counter = counter.clone();
            Interval::new(Duration::from_seconds(5))
                .map(move |()| {
                    counter.fetch_add(1, Ordering::SeqCst);
                })
                .collect::<()>()
        };

        // PollResult for the first time before the timer runs
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut future));
        assert_eq!(0, counter.load(Ordering::SeqCst));

        // Pretend to wait until the next timer
        let first_deadline = TestExecutor::next_timer().expect("Expected a pending timeout (1)");
        assert!(first_deadline >= Duration::from_seconds(5) + start);
        exec.set_fake_time(first_deadline);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut future));
        assert_eq!(1, counter.load(Ordering::SeqCst));

        // PollResulting again before the timer runs shouldn't produce another item from the stream
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut future));
        assert_eq!(1, counter.load(Ordering::SeqCst));

        // "Wait" until the next timeout and poll again: expect another item from the stream
        let second_deadline = TestExecutor::next_timer().expect("Expected a pending timeout (2)");
        exec.set_fake_time(second_deadline);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut future));
        assert_eq!(2, counter.load(Ordering::SeqCst));

        assert_eq!(second_deadline, first_deadline + Duration::from_seconds(5));
    }

    #[test]
    fn timer_fake_time() {
        let mut exec = TestExecutor::new_with_fake_time();
        exec.set_fake_time(Time::from_nanos(0));

        let mut timer = Timer::new(Time::after(Duration::from_seconds(1)));
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut timer));

        exec.set_fake_time(Time::after(Duration::from_seconds(1)));
        assert_eq!(Poll::Ready(()), exec.run_until_stalled(&mut timer));
    }
}
