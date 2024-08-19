// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::signals::{send_signal, SignalDetail, SignalEvent, SignalEventNotify, SignalInfo};
use crate::task::timers::TimerId;
use crate::task::{Kernel, ThreadGroup};
use crate::time::utc;
use crate::timer::Timeline;
use futures::stream::AbortHandle;
use starnix_logging::{log_trace, log_warn, track_stub};
use starnix_sync::Mutex;
use starnix_uapi::errors::Errno;
use starnix_uapi::ownership::{TempRef, WeakRef};
use starnix_uapi::time::{duration_from_timespec, time_from_timespec, timespec_from_duration};
use starnix_uapi::{itimerspec, SI_TIMER};
use std::sync::Arc;
use {fuchsia_async as fasync, fuchsia_zircon as zx};

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

#[derive(Debug)]
pub struct IntervalTimer {
    pub timer_id: TimerId,

    timeline: Timeline,

    pub signal_event: SignalEvent,

    state: Mutex<IntervalTimerMutableState>,
}
pub type IntervalTimerHandle = Arc<IntervalTimer>;

#[derive(Default, Debug)]
struct IntervalTimerMutableState {
    /// Handle to abort the running timer task.
    abort_handle: Option<AbortHandle>,
    /// If the timer is armed (started).
    armed: bool,
    /// Time of the next expiration on the requested timeline.
    target_time: zx::Time,
    /// Interval for periodic timer.
    interval: zx::Duration,
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
        signal_event: SignalEvent,
    ) -> Result<IntervalTimerHandle, Errno> {
        Ok(Arc::new(Self { timer_id, timeline, signal_event, state: Default::default() }))
    }

    fn signal_info(self: &IntervalTimerHandle) -> Option<SignalInfo> {
        let signal_detail = SignalDetail::Timer { timer: self.clone() };
        Some(SignalInfo::new(self.signal_event.signo?, SI_TIMER, signal_detail))
    }

    async fn start_timer_loop(self: &IntervalTimerHandle, thread_group: WeakRef<ThreadGroup>) {
        loop {
            let target_monotonic = loop {
                // We may have to issue multiple sleeps if the target time in the timer is
                // updated while we are sleeping or if our estimation of the target time
                // relative to the monotonic clock is off.
                let target_time = self.state.lock().target_time;
                let target_monotonic = match self.timeline {
                    // TODO(https://fxbug.dev/328306129) handle boot and monotonic time separately
                    Timeline::BootTime | Timeline::Monotonic => target_time,
                    Timeline::RealTime => utc::estimate_monotonic_deadline_from_utc(target_time),
                };
                if zx::Time::get_monotonic() >= target_monotonic {
                    break target_monotonic;
                }
                fasync::Timer::new(target_monotonic).await;
            };

            if !self.state.lock().armed {
                return;
            }

            // Timer expirations are counted as overruns except SIGEV_NONE.
            if self.signal_event.notify != SignalEventNotify::None {
                let mut guard = self.state.lock();
                let overtime = zx::Time::get_monotonic() - target_monotonic;
                // If the `interval` is zero, the timer expires just once, at the time
                // specified by `target_time`.
                if guard.interval == zx::Duration::ZERO {
                    guard.overrun_cur = 1;
                } else {
                    let exp =
                        i32::try_from(overtime.into_nanos() / guard.interval.into_nanos() + 1)
                            .unwrap_or(i32::MAX);
                    guard.overrun_cur = guard.overrun_cur.saturating_add(exp);
                };
            }

            // Check on notify enum to determine the signal target.
            if let Some(thread_group) = thread_group.upgrade() {
                match self.signal_event.notify {
                    SignalEventNotify::Signal => {
                        if let Some(signal_info) = self.signal_info() {
                            log_trace!(
                                signal = signal_info.signal.number(),
                                pid = thread_group.leader,
                                "sending signal for timer"
                            );
                            thread_group.write().send_signal(signal_info);
                        }
                    }
                    SignalEventNotify::None => {}
                    SignalEventNotify::Thread { .. } => {
                        track_stub!(TODO("https://fxbug.dev/322875029"), "SIGEV_THREAD timer");
                    }
                    SignalEventNotify::ThreadId(tid) => {
                        // Check if the target thread exists in the thread group.
                        thread_group.read().get_task(tid).map(TempRef::into_static).map(|target| {
                            if let Some(signal_info) = self.signal_info() {
                                log_trace!(
                                    signal = signal_info.signal.number(),
                                    tid,
                                    "sending signal for timer"
                                );
                                send_signal(&target, signal_info).unwrap_or_else(|e| {
                                    log_warn!("Failed to queue timer signal: {}", e)
                                });
                            }
                        });
                    }
                }
            }

            // If the `interval` is zero, the timer expires just once, at the time
            // specified by `target_time`.
            let mut guard = self.state.lock();
            if guard.interval != zx::Duration::default() {
                guard.target_time = self.timeline.now() + guard.interval;
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
        kernel: &Kernel,
        thread_group: WeakRef<ThreadGroup>,
        new_value: itimerspec,
        is_absolute: bool,
    ) -> Result<(), Errno> {
        let mut guard = self.state.lock();

        let target_time = if is_absolute {
            time_from_timespec(new_value.it_value)?
        } else {
            self.timeline.now() + duration_from_timespec(new_value.it_value)?
        };
        let interval = duration_from_timespec(new_value.it_interval)?;

        // Stop the current running task;
        guard.disarm();

        if target_time == zx::Time::ZERO {
            return Ok(());
        }

        guard.armed = true;
        guard.target_time = target_time;
        guard.interval = interval;
        guard.on_setting_changed();

        let self_ref = self.clone();
        kernel.kthreads.spawn_future(async move {
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

        Ok(())
    }

    pub fn disarm(&self) {
        let mut guard = self.state.lock();
        guard.disarm();
        guard.on_setting_changed();
    }

    pub fn time_remaining(&self) -> TimerRemaining {
        let guard = self.state.lock();
        if !guard.armed {
            return TimerRemaining::default();
        }

        TimerRemaining {
            remainder: guard.target_time - self.timeline.now(),
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
