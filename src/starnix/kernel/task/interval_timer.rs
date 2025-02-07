// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::signals::{send_signal, SignalDetail, SignalEvent, SignalEventNotify, SignalInfo};
use crate::task::{
    CurrentTask, GenericDuration, HrTimer, HrTimerHandle, TargetTime, ThreadGroup, Timeline,
    TimerId, TimerWakeup,
};
use crate::time::utc::estimate_boot_deadline_from_utc;
use crate::vfs::timer::TimerOps;
use assert_matches::assert_matches;

use futures::stream::AbortHandle;
use starnix_logging::{log_error, log_trace, log_warn, track_stub};
use starnix_sync::Mutex;
use starnix_types::ownership::{TempRef, WeakRef};
use starnix_types::time::{duration_from_timespec, timespec_from_duration};
use starnix_uapi::errors::Errno;
use starnix_uapi::{itimerspec, SI_TIMER};
use std::fmt::Debug;
use std::sync::Arc;

#[derive(Default)]
pub struct TimerRemaining {
    /// Remaining time until the next expiration.
    pub remainder: zx::SyntheticDuration,
    /// Interval for periodic timer.
    pub interval: zx::SyntheticDuration,
}

impl From<TimerRemaining> for itimerspec {
    fn from(value: TimerRemaining) -> Self {
        Self {
            it_interval: timespec_from_duration(value.interval),
            it_value: timespec_from_duration(value.remainder),
        }
    }
}

#[derive(Debug)]
pub struct IntervalTimer {
    pub timer_id: TimerId,

    /// HrTimer to trigger wakeup
    hr_timer: Option<HrTimerHandle>,

    timeline: Timeline,

    pub signal_event: SignalEvent,

    state: Mutex<IntervalTimerMutableState>,
}
pub type IntervalTimerHandle = Arc<IntervalTimer>;

#[derive(Debug)]
struct IntervalTimerMutableState {
    /// Handle to abort the running timer task.
    abort_handle: Option<AbortHandle>,
    /// If the timer is armed (started).
    armed: bool,
    /// Time of the next expiration on the requested timeline.
    target_time: TargetTime,
    /// Interval for periodic timer.
    interval: zx::SyntheticDuration,
    /// Number of timer expirations that have occurred since the last time a signal was sent.
    ///
    /// Timer expiration is not counted as overrun under `SignalEventNotify::None`.
    overrun_cur: i32,
    /// Number of timer expirations that was on last delivered signal.
    overrun_last: i32,
}

impl IntervalTimerMutableState {
    fn disarm(&mut self) {
        self.armed = false;
        if let Some(abort_handle) = &self.abort_handle {
            abort_handle.abort();
        }
        self.abort_handle = None;
    }

    fn on_setting_changed(&mut self) {
        self.overrun_cur = 0;
        self.overrun_last = 0;
    }
}

impl IntervalTimer {
    pub fn new(
        timer_id: TimerId,
        timeline: Timeline,
        wakeup_type: TimerWakeup,
        signal_event: SignalEvent,
    ) -> Result<IntervalTimerHandle, Errno> {
        let hr_timer = match wakeup_type {
            TimerWakeup::Regular => None,
            TimerWakeup::Alarm => Some(HrTimer::new()),
        };
        Ok(Arc::new(Self {
            timer_id,
            hr_timer,
            timeline,
            signal_event,
            state: Mutex::new(IntervalTimerMutableState {
                target_time: timeline.zero_time(),
                abort_handle: Default::default(),
                armed: Default::default(),
                interval: Default::default(),
                overrun_cur: Default::default(),
                overrun_last: Default::default(),
            }),
        }))
    }

    fn signal_info(self: &IntervalTimerHandle) -> Option<SignalInfo> {
        let signal_detail = SignalDetail::Timer { timer: self.clone() };
        Some(SignalInfo::new(self.signal_event.signo?, SI_TIMER, signal_detail))
    }

    async fn start_timer_loop(
        self: &IntervalTimerHandle,
        system_task: &CurrentTask,
        timer_thread_group: WeakRef<ThreadGroup>,
    ) {
        loop {
            let overtime = loop {
                // We may have to issue multiple sleeps if the target time in the timer is
                // updated while we are sleeping or if our estimation of the target time
                // relative to the monotonic clock is off. Drop the guard before blocking so
                // that the target time can be updated.
                let target_time = { self.state.lock().target_time };
                let now = self.timeline.now();
                if now >= target_time {
                    break now
                        .delta(&target_time)
                        .expect("timer timeline and target time are comparable");
                }
                if let Some(hr_timer) = &self.hr_timer {
                    assert_matches!(
                        target_time,
                        TargetTime::BootInstant(_) | TargetTime::RealTime(_),
                        "monotonic times can't be alarm deadlines",
                    );
                    if let Err(e) = hr_timer.start(system_task, None, target_time) {
                        log_error!("Failed to start the HrTimer to trigger wakeup: {e}");
                    }
                }

                match target_time {
                    TargetTime::Monotonic(t) => fuchsia_async::Timer::new(t).await,
                    TargetTime::BootInstant(t) => fuchsia_async::Timer::new(t).await,
                    TargetTime::RealTime(t) => {
                        fuchsia_async::Timer::new(estimate_boot_deadline_from_utc(t)).await
                    }
                }
            };
            if !self.state.lock().armed {
                return;
            }

            // Timer expirations are counted as overruns except SIGEV_NONE.
            if self.signal_event.notify != SignalEventNotify::None {
                let mut guard = self.state.lock();
                // If the `interval` is zero, the timer expires just once, at the time
                // specified by `target_time`.
                if guard.interval == zx::SyntheticDuration::ZERO {
                    guard.overrun_cur = 1;
                } else {
                    let exp =
                        i32::try_from(overtime.into_nanos() / guard.interval.into_nanos() + 1)
                            .unwrap_or(i32::MAX);
                    guard.overrun_cur = guard.overrun_cur.saturating_add(exp);
                };
            }

            // Check on notify enum to determine the signal target.
            if let Some(timer_thread_group) = timer_thread_group.upgrade() {
                match self.signal_event.notify {
                    SignalEventNotify::Signal => {
                        if let Some(signal_info) = self.signal_info() {
                            log_trace!(
                                signal = signal_info.signal.number(),
                                pid = timer_thread_group.leader;
                                "sending signal for timer"
                            );
                            timer_thread_group.write().send_signal(signal_info);
                        }
                    }
                    SignalEventNotify::None => {}
                    SignalEventNotify::Thread { .. } => {
                        track_stub!(TODO("https://fxbug.dev/322875029"), "SIGEV_THREAD timer");
                    }
                    SignalEventNotify::ThreadId(tid) => {
                        // Check if the target thread exists in the thread group.
                        timer_thread_group.read().get_task(tid).map(TempRef::into_static).map(
                            |target| {
                                if let Some(signal_info) = self.signal_info() {
                                    log_trace!(
                                        signal = signal_info.signal.number(),
                                        tid;
                                        "sending signal for timer"
                                    );
                                    send_signal(&target, signal_info).unwrap_or_else(|e| {
                                        log_warn!("Failed to queue timer signal: {}", e)
                                    });
                                }
                            },
                        );
                    }
                }
            }

            // If the `interval` is zero, the timer expires just once, at the time
            // specified by `target_time`.
            let mut guard = self.state.lock();
            if guard.interval != zx::SyntheticDuration::default() {
                guard.target_time = self.timeline.now() + GenericDuration::from(guard.interval);
            } else {
                guard.disarm();
                return;
            }
        }
    }

    pub fn on_signal_delivered(self: &IntervalTimerHandle) {
        let mut guard = self.state.lock();
        guard.overrun_last = guard.overrun_cur;
        guard.overrun_cur = 0;
    }

    pub fn arm(
        self: &IntervalTimerHandle,
        current_task: &CurrentTask,
        new_value: itimerspec,
        is_absolute: bool,
    ) -> Result<(), Errno> {
        let mut guard = self.state.lock();

        let target_time = if is_absolute {
            self.timeline.target_from_timespec(new_value.it_value)?
        } else {
            self.timeline.now()
                + GenericDuration::from(duration_from_timespec::<zx::SyntheticTimeline>(
                    new_value.it_value,
                )?)
        };
        let interval = duration_from_timespec(new_value.it_interval)?;

        if let Some(hr_timer) = &self.hr_timer {
            *hr_timer.is_interval.lock() = guard.interval != zx::SyntheticDuration::default();
        }

        // Stop the current running task;
        guard.disarm();

        if target_time.is_zero() {
            return Ok(());
        }

        guard.armed = true;
        guard.target_time = target_time;
        guard.interval = interval;
        guard.on_setting_changed();

        let kernel_ref = current_task.kernel();
        let self_ref = self.clone();
        let thread_group = current_task.thread_group.weak_thread_group.clone();
        current_task.kernel().kthreads.spawn_future(async move {
            let _ = {
                // 1. Lock the state to update `abort_handle` when the timer is still armed.
                // 2. MutexGuard needs to be dropped before calling await on the future task.
                // Unfortuately, std::mem::drop is not working correctly on this:
                // (https://github.com/rust-lang/rust/issues/57478).
                let mut guard = self_ref.state.lock();
                if !guard.armed {
                    return;
                }

                let (abortable_future, abort_handle) = futures::future::abortable(
                    self_ref.start_timer_loop(kernel_ref.kthreads.system_task(), thread_group),
                );
                guard.abort_handle = Some(abort_handle);
                abortable_future
            }
            .await;
        });

        Ok(())
    }

    pub fn disarm(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        let mut guard = self.state.lock();
        guard.disarm();
        guard.on_setting_changed();
        if let Some(hr_timer) = &self.hr_timer {
            hr_timer.stop(current_task)?;
        }
        Ok(())
    }

    pub fn time_remaining(&self) -> TimerRemaining {
        let guard = self.state.lock();
        if !guard.armed {
            return TimerRemaining::default();
        }

        TimerRemaining {
            remainder: std::cmp::max(
                zx::SyntheticDuration::ZERO,
                *guard.target_time.delta(&self.timeline.now()).expect("timelines must match"),
            ),
            interval: guard.interval,
        }
    }

    pub fn overrun_cur(&self) -> i32 {
        self.state.lock().overrun_cur
    }
    pub fn overrun_last(&self) -> i32 {
        self.state.lock().overrun_last
    }
}

impl PartialEq for IntervalTimer {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::addr_of!(self) == std::ptr::addr_of!(other)
    }
}
impl Eq for IntervalTimer {}
