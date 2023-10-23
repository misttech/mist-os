// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    lock::Mutex,
    logging::not_implemented,
    signals::{send_signal, SignalDetail, SignalEvent, SignalEventNotify, SignalInfo},
    task::{
        timers::{ClockId, TimerId},
        ThreadGroup,
    },
    time::utc,
    types::*,
};

use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::stream::AbortHandle;
use std::sync::{Arc, Weak};

#[derive(Default)]
pub struct TimerRemaining {
    /// Remaining time until the next expiration.
    pub remainder: zx::Duration,
    /// Interval for periodic timer.
    pub interval: zx::Duration,
}

impl From<TimerRemaining> for itimerspec {
    fn from(value: TimerRemaining) -> Self {
        Self {
            it_interval: timespec_from_duration(value.interval),
            it_value: timespec_from_duration(value.remainder),
        }
    }
}

#[allow(dead_code)] // TODO(fxb/123084)
#[derive(Debug)]
pub struct IntervalTimer {
    timer_id: TimerId,

    clock_id: ClockId,

    signal_event: SignalEvent,

    state: Mutex<IntervalTimerMutableState>,
}
pub type IntervalTimerHandle = Arc<IntervalTimer>;

#[allow(dead_code)] // TODO(fxb/123084)
#[derive(Default, Debug)]
struct IntervalTimerMutableState {
    /// Handle to abort the running timer task.
    abort_handle: Option<AbortHandle>,
    /// If the timer is armed (started).
    armed: bool,
    /// Absolute UTC time of the next expiration.
    target_time: zx::Time,
    /// Interval for periodic timer.
    interval: zx::Duration,
    /// Number of overruns that have occurred since the last time a signal was sent.
    overrun: u64,
    /// Number of overruns that was on last delivered signal.
    overrun_last: u64,
    /// Whether or not the timer is waiting for being requeued.
    ///
    /// If true, a signal to `target` is already queued, and timer expirations should increment
    /// `overrun` instead of sending another signal.
    requeue_pending: bool,
}

impl IntervalTimerMutableState {
    fn disarm(&mut self) {
        self.armed = false;
        if let Some(abort_handle) = &self.abort_handle {
            abort_handle.abort();
        }
        self.abort_handle = None;
    }
}

impl IntervalTimer {
    pub fn new(
        timer_id: TimerId,
        clock_id: ClockId,
        signal_event: SignalEvent,
    ) -> IntervalTimerHandle {
        Arc::new(Self { timer_id, clock_id, signal_event, state: Default::default() })
    }

    fn signal_info(&self) -> Option<SignalInfo> {
        let signal_detail = SignalDetail::Timer {
            timer_id: self.timer_id,
            overrun: 0,
            sigval: self.signal_event.value?,
        };
        Some(SignalInfo::new(self.signal_event.signo?, SI_TIMER as u32, signal_detail))
    }

    async fn start_timer_loop(&self, thread_group: Weak<ThreadGroup>) {
        loop {
            loop {
                // We may have to issue multiple sleeps if the target time in the timer is
                // updated while we are sleeping or if our estimation of the target time
                // relative to the monotonic clock is off.
                let target_monotonic =
                    utc::estimate_monotonic_deadline_from_utc(self.state.lock().target_time);
                if zx::Time::get_monotonic() >= target_monotonic {
                    break;
                }
                fuchsia_async::Timer::new(target_monotonic).await;
            }

            if !self.state.lock().armed {
                return;
            }

            // Check on notify enum to determine the signal target.
            if let Some(thread_group) = thread_group.upgrade() {
                let signal_target = match self.signal_event.notify {
                    SignalEventNotify::Signal => self.signal_event.signo.and_then(|signal| {
                        thread_group
                            .read()
                            .get_signal_target(&signal.into())
                            .map(TempRef::into_static)
                    }),
                    SignalEventNotify::None => None, // No need to do anything.
                    SignalEventNotify::Thread { .. } => {
                        not_implemented!("SIGEV_THREAD timer");
                        None
                    }
                    SignalEventNotify::ThreadId(tid) => {
                        // Check if the target thread exists in the thread group.
                        thread_group.read().get_task(tid).map(TempRef::into_static)
                    }
                };

                if let Some(target) = &signal_target {
                    if let Some(signal_info) = self.signal_info() {
                        send_signal(target, signal_info)
                    }
                }
            }

            // If the `interval` is zero, the timer expires just once, at the time
            // specified by `target_time`.
            let mut guard = self.state.lock();
            if guard.interval != zx::Duration::default() {
                guard.target_time = utc::utc_now() + guard.interval;
            } else {
                guard.disarm();
                return;
            }
        }
    }

    pub fn arm(
        self: &IntervalTimerHandle,
        thread_group: Weak<ThreadGroup>,
        executor: &fasync::EHandle,
        target_time: zx::Time,
        interval: zx::Duration,
    ) {
        let mut guard = self.state.lock();

        // Stop the current running task;
        guard.disarm();

        if target_time == zx::Time::ZERO {
            return;
        }

        guard.armed = true;
        guard.target_time = target_time;
        guard.interval = interval;
        guard.overrun = 0;
        guard.overrun_last = 0;

        // TODO(fxb/123084): check on clock_id to see if the clock supports creating a timer.

        let self_ref = self.clone();
        executor.spawn_detached(async move {
            let _ = {
                // 1. Lock the state to update `abort_handle` when the timer is still armed.
                // 2. MutexGuard needs to be dropped before calling await on the future task.
                // Unfortuately, std::mem::drop is not working correctly on this:
                // (https://github.com/rust-lang/rust/issues/57478).
                let mut guard = self_ref.state.lock();
                if !guard.armed {
                    return;
                }

                let (abortable_future, abort_handle) =
                    futures::future::abortable(self_ref.start_timer_loop(thread_group));
                guard.abort_handle = Some(abort_handle);
                abortable_future
            }
            .await;
        });
    }

    pub fn disarm(&self) {
        let mut guard = self.state.lock();
        guard.disarm();
    }

    pub fn time_remaining(&self) -> TimerRemaining {
        let guard = self.state.lock();
        if !guard.armed {
            return TimerRemaining::default();
        }

        TimerRemaining { remainder: guard.target_time - utc::utc_now(), interval: guard.interval }
    }

    pub fn overrun_last(&self) -> i32 {
        self.state.lock().overrun_last as i32
    }
}
