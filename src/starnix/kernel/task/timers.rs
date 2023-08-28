// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;

use fuchsia_sync::Mutex;

use crate::{
    signals::{SignalEvent, SignalEventNotify, SignalEventValue},
    task::interval_timer::{IntervalTimer, IntervalTimerHandle},
    types::*,
};

// Table for POSIX timers from timer_create() that deliver timers via signals (not new-style
// timerfd's).
//
// This is currently unimplemented.
#[derive(Debug, Default)]
pub struct TimerTable {
    state: Mutex<TimerTableMutableState>,
}

#[derive(Debug, Default)]
struct TimerTableMutableState {
    /// The `TimerId` at which allocation should begin searching for an unused ID.
    next_timer_id: TimerId,
    timers: HashMap<TimerId, IntervalTimerHandle>,
}

pub type TimerId = __kernel_timer_t;
pub type ClockId = uapi::__kernel_clockid_t;

impl TimerTable {
    pub fn new() -> TimerTable {
        Default::default()
    }

    /// Creates a new per-process interval timer.
    ///
    /// The new timer is initially disarmed.
    pub fn create(&self, clock_id: ClockId, sigev: Option<sigevent>) -> Result<TimerId, Errno> {
        let mut state = self.state.lock();

        // Find a vacant timer id.
        let end = state.next_timer_id;
        let timer_id = loop {
            let timer_id = state.next_timer_id;
            state.next_timer_id += 1;

            if state.next_timer_id == TimerId::MAX {
                state.next_timer_id = 0;
            } else if state.next_timer_id == end {
                // After searching the entire timer map, there is no vacant timer id.
                // Fails the call and implies the program could try it again later.
                return error!(EAGAIN);
            }

            if !state.timers.contains_key(&timer_id) {
                break timer_id;
            }
        };

        let signal_event: SignalEvent = match sigev {
            Some(sigev) => sigev.try_into()?,
            None => SignalEvent::new(
                SignalEventValue(timer_id as u64),
                SIGALRM,
                SignalEventNotify::Signal,
            ),
        };

        state.timers.insert(timer_id, IntervalTimer::new(timer_id, clock_id, signal_event));

        Ok(timer_id)
    }

    /// Disarms and deletes a timer.
    pub fn delete(&self, id: TimerId) -> Result<(), Errno> {
        let mut state = self.state.lock();
        match state.timers.remove_entry(&id) {
            Some(entry) => {
                entry.1.disarm();
                Ok(())
            }
            None => error!(EINVAL),
        }
    }

    /// Fetches the time remaining until the next expiration of a timer, along with the interval
    /// setting of the timer.
    pub fn get_time(&self, _id: TimerId) -> Result<itimerspec, Errno> {
        error!(ENOSYS)
    }

    /// Returns the overrun count for the last timer expiration.
    pub fn get_overrun(&self, _id: TimerId) -> Result<i32, Errno> {
        error!(ENOSYS)
    }

    /// Arms (start) or disarms (stop) the timer identifierd by `id`. The `new_value` arg is a
    /// pointer to an `itimerspec` structure that specifies the new initial value and the new
    /// interval for the timer.
    pub fn set_time(
        &self,
        _id: TimerId,
        _flags: i32,
        _new_value: UserRef<itimerspec>,
        _old_value: UserRef<itimerspec>,
    ) -> Result<(), Errno> {
        // TODO(fxbug.dev/123084): Really implement.
        Ok(())
    }

    pub fn get_timer(&self, id: TimerId) -> Result<IntervalTimerHandle, Errno> {
        match self.state.lock().timers.get(&id) {
            Some(itimer) => Ok(itimer.clone()),
            None => error!(EINVAL),
        }
    }
}
