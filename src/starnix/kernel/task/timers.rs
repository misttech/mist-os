// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//use fuchsia_zircon as zx;
use std::sync::Arc;

use crate::types::*;

// Table for POSIX timers from timer_create() that deliver timers via signals (not new-style
// timerfd's).
//
// This is currently unimplemented.
#[derive(Debug, Default)]
pub struct TimerTable {}

pub type TimerId = usize;

impl TimerTable {
    pub fn new() -> Arc<TimerTable> {
        Default::default()
    }

    /// Creates a new per-process interval timer.
    pub fn create(
        &self,
        _clockid: uapi::__kernel_clockid_t,
        _event: &sigevent,
    ) -> Result<TimerId, Errno> {
        // TODO(fxbug.dev/123084): Really implement.
        Ok(1)
    }

    /// Disarms and deletes a timer.
    pub fn delete(&self, _id: TimerId) -> Result<(), Errno> {
        // TODO(fxbug.dev/123084): Really implement.
        Ok(())
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
}
