// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_runtime::zx_utc_reference_get;
use fuchsia_zircon as zx;
use fuchsia_zircon::{AsHandleRef, Clock, Unowned};
use std::sync::Arc;
use zerocopy::AsBytes;

use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::*;
use crate::lock::Mutex;
use crate::task::*;
use crate::types::*;

/// Clock types supported by TimerFiles.
pub enum TimerFileClock {
    Monotonic,
    Realtime,
}

/// A `TimerFile` represents a file created by `timerfd_create`.
///
/// Clients can read the number of times the timer has triggered from the file. The file supports
/// blocking reads, waiting for the timer to trigger.
pub struct TimerFile {
    /// The timer that is used to wait for blocking reads.
    timer: Arc<zx::Timer>,

    /// The type of clock this file was created with.
    clock: TimerFileClock,

    /// The deadline (`zx::Time`) for the next timer trigger, and the associated interval
    /// (`zx::Duration`).
    ///
    /// When the file is read, the deadline is recomputed based on the current time and the set
    /// interval. If the interval is 0, `self.timer` is cancelled after the file is read.
    deadline_interval: Mutex<(zx::Time, zx::Duration)>,
}

impl TimerFile {
    /// Creates a new anonymous `TimerFile` in `kernel`.
    ///
    /// Returns an error if the `zx::Timer` could not be created.
    pub fn new_file(
        current_task: &CurrentTask,
        clock: TimerFileClock,
        flags: OpenFlags,
    ) -> Result<FileHandle, Errno> {
        let timer = Arc::new(zx::Timer::create());

        Ok(Anon::new_file(
            current_task,
            Box::new(TimerFile {
                timer,
                clock,
                deadline_interval: Mutex::new((zx::Time::default(), zx::Duration::default())),
            }),
            flags,
        ))
    }

    /// Returns the current `itimerspec` for the file.
    ///
    /// The returned `itimerspec.it_value` contains the amount of time remaining until the
    /// next timer trigger.
    pub fn current_timer_spec(&self) -> itimerspec {
        let (deadline, interval) = *self.deadline_interval.lock();

        let now = zx::Time::get_monotonic();
        let remaining_time = if interval == zx::Duration::default() && deadline <= now {
            timespec_from_duration(zx::Duration::default())
        } else {
            timespec_from_duration(deadline - now)
        };

        itimerspec { it_interval: timespec_from_duration(interval), it_value: remaining_time }
    }

    /// Sets the `itimerspec` for the timer, which will either update the associated `zx::Timer`'s
    /// scheduled trigger or cancel the timer.
    ///
    /// Returns the previous `itimerspec` on success.
    pub fn set_timer_spec(&self, timer_spec: itimerspec, flags: u32) -> Result<itimerspec, Errno> {
        let mut deadline_interval = self.deadline_interval.lock();
        let (old_deadline, old_interval) = *deadline_interval;
        let old_itimerspec = itimerspec_from_deadline_interval(old_deadline, old_interval);

        if timespec_is_zero(timer_spec.it_value) {
            // Sayeth timerfd_settime(2):
            // Setting both fields of new_value.it_value to zero disarms the timer.
            self.timer.cancel().map_err(|status| from_status_like_fdio!(status))?;
        } else {
            let now_monotonic = zx::Time::get_monotonic();
            let new_deadline = if flags & TFD_TIMER_ABSTIME != 0 {
                // If the time_spec represents an absolute time, then treat the
                // `it_value` as the deadline..
                match &self.clock {
                    TimerFileClock::Realtime => {
                        // Since Zircon does not have realtime timers, compute what the value would
                        // be in the monotonic clock assuming realtime progresses linearly.
                        //
                        // TODO(fxbug.dev/117507) implement proper realtime timers that will work
                        // when the realtime clock changes, and implement TFD_TIMER_CANCEL_ON_SET.
                        let utc_clock: Unowned<'static, Clock> = unsafe {
                            let handle = zx_utc_reference_get();
                            Unowned::from_raw_handle(handle)
                        };
                        let utc_details = utc_clock
                            .get_details()
                            .map_err(|status| from_status_like_fdio!(status))?;
                        utc_details
                            .mono_to_synthetic
                            .apply_inverse(time_from_timespec(timer_spec.it_value)?)
                    }
                    TimerFileClock::Monotonic => time_from_timespec(timer_spec.it_value)?,
                }
            } else {
                // .. otherwise the deadline is computed relative to the current time. Without
                // realtime timers in Zircon, we assume realtime and monotonic time progresses the
                // same so relative values need no separate handling.
                let duration = duration_from_timespec(timer_spec.it_value)?;
                now_monotonic + duration
            };
            let new_interval = duration_from_timespec(timer_spec.it_interval)?;

            self.timer
                .set(new_deadline, zx::Duration::default())
                .map_err(|status| from_status_like_fdio!(status))?;
            *deadline_interval = (new_deadline, new_interval);
        }

        Ok(old_itimerspec)
    }

    /// Returns the `zx::Signals` to listen for given `events`. Used to wait on a `zx::Timer`
    /// associated with a `TimerFile`.
    fn get_signals_from_events(events: FdEvents) -> zx::Signals {
        if events.contains(FdEvents::POLLIN) {
            zx::Signals::TIMER_SIGNALED
        } else {
            zx::Signals::NONE
        }
    }

    fn get_events_from_signals(signals: zx::Signals) -> FdEvents {
        let mut events = FdEvents::empty();

        if signals.contains(zx::Signals::TIMER_SIGNALED) {
            events |= FdEvents::POLLIN;
        }

        events
    }
}

impl FileOps for TimerFile {
    fileops_impl_nonseekable!();
    fn write(
        &self,
        file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        // The expected error seems to vary depending on the open flags..
        if file.flags().contains(OpenFlags::NONBLOCK) {
            error!(EINVAL)
        } else {
            error!(ESPIPE)
        }
    }

    fn read(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(current_task, FdEvents::POLLIN | FdEvents::POLLHUP, None, || {
            let mut deadline_interval = self.deadline_interval.lock();
            let (deadline, interval) = *deadline_interval;

            if deadline == zx::Time::default() {
                // The timer has not been set.
                return error!(EAGAIN);
            }

            let now = zx::Time::get_monotonic();
            if deadline > now {
                // The next deadline has not yet passed.
                return error!(EAGAIN);
            }

            let count: i64 = if interval > zx::Duration::default() {
                let elapsed_nanos = (now - deadline).into_nanos();
                // The number of times the timer has triggered is written to `data`.
                let num_intervals = elapsed_nanos / interval.into_nanos() + 1;
                let new_deadline = deadline + interval * num_intervals;

                // The timer is set to clear the `ZX_TIMER_SIGNALED` signal until the next deadline is
                // reached.
                self.timer
                    .set(new_deadline, zx::Duration::default())
                    .map_err(|status| from_status_like_fdio!(status))?;

                // Update the stored deadline.
                *deadline_interval = (new_deadline, interval);

                num_intervals
            } else {
                // The timer is non-repeating, so cancel the timer to clear the `ZX_TIMER_SIGNALED`
                // signal.
                *deadline_interval = (zx::Time::default(), interval);
                self.timer.cancel().map_err(|status| from_status_like_fdio!(status))?;
                1
            };

            Ok(BlockableOpsResult::Done(data.write(count.as_bytes())?))
        })
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        let signal_handler = move |signals: zx::Signals| {
            let events = TimerFile::get_events_from_signals(signals);
            handler(events);
        };
        let canceler = waiter
            .wake_on_zircon_signals(
                self.timer.as_ref(),
                TimerFile::get_signals_from_events(events),
                Box::new(signal_handler),
            )
            .unwrap(); // TODO return error
        let timer = Arc::downgrade(&self.timer);
        Some(WaitCanceler::new(move || {
            if let Some(timer) = timer.upgrade() {
                canceler.cancel(timer.as_handle_ref())
            } else {
                false
            }
        }))
    }

    fn query_events(&self, _current_task: &CurrentTask) -> FdEvents {
        let observed = match self.timer.wait_handle(zx::Signals::TIMER_SIGNALED, zx::Time::ZERO) {
            Err(zx::Status::TIMED_OUT) => zx::Signals::empty(),
            res => res.unwrap(),
        };
        TimerFile::get_events_from_signals(observed)
    }
}
