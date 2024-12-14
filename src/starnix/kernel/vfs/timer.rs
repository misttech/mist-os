// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::fuchsia::{BootZxTimer, MonotonicZxTimer};
use crate::power::OnWakeOps;
use crate::task::{
    CurrentTask, EventHandler, GenericDuration, HandleWaitCanceler, HrTimer, SignalHandler,
    SignalHandlerInner, TargetTime, Timeline, TimerWakeup, WaitCanceler, Waiter,
};
use crate::vfs::buffers::{InputBuffer, OutputBuffer};
use crate::vfs::{
    fileops_impl_nonseekable, fileops_impl_noop_sync, Anon, FileHandle, FileObject, FileOps,
};
use starnix_logging::log_warn;
use starnix_sync::{FileOpsCore, Locked, Mutex};
use starnix_types::time::{duration_from_timespec, timespec_from_duration, timespec_is_zero};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{error, itimerspec, TFD_TIMER_ABSTIME};
use std::sync::{Arc, Weak};
use zerocopy::IntoBytes;
use zx::{self as zx, HandleRef};

pub trait TimerOps: Send + Sync + 'static {
    /// Starts the timer with the specified `deadline`.
    ///
    /// This method should start the timer and schedule it to trigger at the specified `deadline`.
    /// The timer should be cancelled if it is already running.
    fn start(
        &self,
        current_task: &CurrentTask,
        source: Option<Weak<dyn OnWakeOps>>,
        deadline: TargetTime,
    ) -> Result<(), Errno>;

    /// Stops the timer.
    ///
    /// This method should stop the timer and prevent it from triggering.
    fn stop(&self, current_task: &CurrentTask) -> Result<(), Errno>;

    /// Creates a `WaitCanceler` that can be used to wait for the timer to be cancelled.
    fn wait_canceler(&self, canceler: HandleWaitCanceler) -> WaitCanceler;

    /// Returns a reference to the underlying Zircon handle.
    fn as_handle_ref(&self) -> HandleRef<'_>;
}

/// A `TimerFile` represents a file created by `timerfd_create`.
///
/// Clients can read the number of times the timer has triggered from the file. The file supports
/// blocking reads, waiting for the timer to trigger.
pub struct TimerFile {
    /// The timer that is used to wait for blocking reads.
    timer: Arc<dyn TimerOps>,

    /// The type of clock this file was created with.
    timeline: Timeline,

    /// The deadline (`TargetTime`) for the next timer trigger, and the associated interval
    /// (`zx::MonotonicDuration`).
    ///
    /// When the file is read, the deadline is recomputed based on the current time and the set
    /// interval. If the interval is 0, `self.timer` is cancelled after the file is read.
    deadline_interval: Mutex<(TargetTime, zx::MonotonicDuration)>,
}

impl TimerFile {
    /// Creates a new anonymous `TimerFile` in `kernel`.
    ///
    /// Returns an error if the `zx::Timer` could not be created.
    pub fn new_file(
        current_task: &CurrentTask,
        wakeup_type: TimerWakeup,
        timeline: Timeline,
        flags: OpenFlags,
    ) -> Result<FileHandle, Errno> {
        let timer: Arc<dyn TimerOps> = match (wakeup_type, timeline) {
            (TimerWakeup::Regular, Timeline::Monotonic) => Arc::new(MonotonicZxTimer::new()),
            (TimerWakeup::Regular, Timeline::BootInstant | Timeline::RealTime) => {
                Arc::new(BootZxTimer::new())
            }
            (TimerWakeup::Alarm, Timeline::BootInstant | Timeline::RealTime) => {
                Arc::new(HrTimer::new())
            }
            (TimerWakeup::Alarm, Timeline::Monotonic) => {
                unreachable!("monotonic times cannot be alarm deadlines")
            }
        };

        Ok(Anon::new_file(
            current_task,
            Box::new(TimerFile {
                timer,
                timeline,
                deadline_interval: Mutex::new((
                    timeline.zero_time(),
                    zx::MonotonicDuration::default(),
                )),
            }),
            flags,
            "[timerfd]",
        ))
    }

    /// Returns the current `itimerspec` for the file.
    ///
    /// The returned `itimerspec.it_value` contains the amount of time remaining until the
    /// next timer trigger.
    pub fn current_timer_spec(&self) -> itimerspec {
        let (deadline, interval) = *self.deadline_interval.lock();

        let now = self.timeline.now();
        let remaining_time = if interval == zx::MonotonicDuration::default() && deadline <= now {
            timespec_from_duration(zx::MonotonicDuration::default())
        } else {
            timespec_from_duration(
                *deadline.delta(&now).expect("deadline and now come from same timeline"),
            )
        };

        itimerspec { it_interval: timespec_from_duration(interval), it_value: remaining_time }
    }

    /// Sets the `itimerspec` for the timer, which will either update the associated `zx::Timer`'s
    /// scheduled trigger or cancel the timer.
    ///
    /// Returns the previous `itimerspec` on success.
    pub fn set_timer_spec(
        &self,
        current_task: &CurrentTask,
        file_object: &FileObject,
        timer_spec: itimerspec,
        flags: u32,
    ) -> Result<itimerspec, Errno> {
        let mut deadline_interval = self.deadline_interval.lock();
        let (old_deadline, old_interval) = *deadline_interval;
        let old_itimerspec = old_deadline.itimerspec(old_interval);

        if timespec_is_zero(timer_spec.it_value) {
            // Sayeth timerfd_settime(2):
            // Setting both fields of new_value.it_value to zero disarms the timer.
            *deadline_interval = (self.timeline.zero_time(), zx::MonotonicDuration::ZERO);
            self.timer.stop(current_task)?;
        } else {
            let new_deadline = if flags & TFD_TIMER_ABSTIME != 0 {
                // If the time_spec represents an absolute time, then treat the
                // `it_value` as the deadline..
                self.timeline.target_from_timespec(timer_spec.it_value)?
            } else {
                // .. otherwise the deadline is computed relative to the current time.
                self.timeline.now()
                    + GenericDuration::from(duration_from_timespec::<zx::SyntheticTimeline>(
                        timer_spec.it_value,
                    )?)
            };
            let new_interval = duration_from_timespec(timer_spec.it_interval)?;

            self.timer.start(current_task, Some(file_object.weak_handle.clone()), new_deadline)?;
            *deadline_interval = (new_deadline, new_interval);
        }

        Ok(old_itimerspec)
    }

    /// Returns the `zx::Signals` to listen for given `events`. Used to wait on the `TimerOps`
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
    fileops_impl_noop_sync!();

    fn close(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) {
        if let Err(e) = self.timer.stop(current_task) {
            log_warn!("Failed to stop the timer when closing the timerfd: {e:?}");
        }
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
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
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(locked, current_task, FdEvents::POLLIN | FdEvents::POLLHUP, None, |_| {
            let mut deadline_interval = self.deadline_interval.lock();
            let (deadline, interval) = *deadline_interval;

            if deadline.is_zero() {
                // The timer has not been set.
                return error!(EAGAIN);
            }

            let now = self.timeline.now();
            if deadline > now {
                // The next deadline has not yet passed.
                return error!(EAGAIN);
            }

            let count: i64 = if interval > zx::MonotonicDuration::default() {
                let elapsed_nanos =
                    now.delta(&deadline).expect("timelines must match").into_nanos();
                // The number of times the timer has triggered is written to `data`.
                let num_intervals = elapsed_nanos / interval.into_nanos() + 1;
                let new_deadline = deadline + GenericDuration::from(interval * num_intervals);

                // The timer is set to clear the `ZX_TIMER_SIGNALED` signal until the next deadline
                // is reached.
                self.timer.start(current_task, Some(file.weak_handle.clone()), new_deadline)?;

                // Update the stored deadline.
                *deadline_interval = (new_deadline, interval);

                num_intervals
            } else {
                // The timer is non-repeating, so cancel the timer to clear the `ZX_TIMER_SIGNALED`
                // signal.
                *deadline_interval = (self.timeline.zero_time(), zx::MonotonicDuration::ZERO);
                self.timer.stop(current_task)?;
                1
            };

            data.write(count.as_bytes())
        })
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        event_handler: EventHandler,
    ) -> Option<WaitCanceler> {
        let signal_handler = SignalHandler {
            inner: SignalHandlerInner::ZxHandle(TimerFile::get_events_from_signals),
            event_handler,
            err_code: None,
        };
        let canceler = waiter
            .wake_on_zircon_signals(
                &self.timer.as_handle_ref(),
                TimerFile::get_signals_from_events(events),
                signal_handler,
            )
            .unwrap(); // TODO return error
        let wait_canceler = self.timer.wait_canceler(canceler);
        Some(wait_canceler)
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let observed =
            match self.timer.as_handle_ref().wait(zx::Signals::TIMER_SIGNALED, zx::Instant::ZERO) {
                Err(zx::Status::TIMED_OUT) => zx::Signals::empty(),
                res => res.unwrap(),
            };
        Ok(TimerFile::get_events_from_signals(observed))
    }
}
