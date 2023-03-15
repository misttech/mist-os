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

    pub fn create(
        &self,
        _clockid: uapi::__kernel_clockid_t,
        _event: &sigevent,
    ) -> Result<TimerId, Errno> {
        // TODO(fxbug.dev/123084): Really implement.
        Ok(1)
    }

    pub fn delete(&self, _id: TimerId) -> Result<(), Errno> {
        // TODO(fxbug.dev/123084): Really implement.
        Ok(())
    }

    pub fn get_time(&self, _id: TimerId) -> Result<itimerspec, Errno> {
        error!(ENOSYS)
    }

    pub fn get_overrun(&self, _id: TimerId) -> Result<i32, Errno> {
        error!(ENOSYS)
    }
}
