// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    lock::Mutex,
    signals::{send_signal, SignalDetail, SignalEvent, SignalInfo},
    task::{
        timers::{ClockId, TimerId},
        ThreadGroup,
    },
    time::utc,
    types::*,
};

use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use std::sync::{Arc, Weak};

#[derive(Default)]
pub struct TimerRemaining {
    /// Remaining time until the next expiration.
    pub remainder: zx::Duration,
    /// Interval for periodic timer.
    pub interval: zx::Duration,
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
    /// If the timer is armed (started).
    armed: bool,
    /// Absolute time of the next expiration.
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

    pub fn arm(
        self: &IntervalTimerHandle,
        thread_group: Weak<ThreadGroup>,
        executor: &fasync::EHandle,
        target_time: zx::Time,
        interval: zx::Duration,
    ) {
        {
            let mut guard = self.state.lock();
            guard.armed = true;
            guard.target_time = target_time;
            guard.interval = interval;
            guard.overrun = 0;
            guard.overrun_last = 0;
        }
        let self_ref = self.clone();

        // TODO(fxb/123084): check on clock_id to see if the clock supports creating a timer.

        fasync::Task::spawn_on(executor, async move {
            loop {
                loop {
                    // We may have to issue multiple sleeps if the target time in the timer is
                    // updated while we are sleeping or if our estimation of the target time
                    // relative to the monotonic clock is off.
                    let target_monotonic = utc::estimate_monotonic_deadline_from_utc(
                        self_ref.state.lock().target_time,
                    );
                    if zx::Time::get_monotonic() >= target_monotonic {
                        break;
                    }
                    fuchsia_async::Timer::new(target_monotonic).await;
                }
                if !self_ref.state.lock().armed {
                    return;
                }
                if let Some(thread_group) = thread_group.upgrade() {
                    if let Some(signal_info) = &self_ref.signal_info() {
                        // TODO(fxb/123084): Check on signal_notify to determine the target.
                        let signal_target = thread_group
                            .read()
                            .get_signal_target(&signal_info.signal.into())
                            .map(|task| {
                                // SAFETY: signal_target is kept on the stack. The static is
                                // required to ensure the lock on ThreadGroup can be dropped.
                                unsafe { TempRef::into_static(task) }
                            });
                        if let Some(task) = &signal_target {
                            send_signal(task, signal_info.clone());
                        }
                    }
                    let mut guard = self_ref.state.lock();
                    if guard.interval != zx::Duration::default() {
                        guard.target_time = utc::utc_now() + guard.interval;
                    } else {
                        guard.armed = false;
                        break;
                    }
                } else {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn disarm(&self) {
        self.state.lock().armed = false;
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
