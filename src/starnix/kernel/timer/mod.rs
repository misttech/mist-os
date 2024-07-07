// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::time::utc;
use fuchsia_zircon as zx;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Timeline {
    RealTime,
    Monotonic,
    BootTime,
}

impl Timeline {
    /// Returns the current time on this timeline.
    pub fn now(&self) -> zx::Time {
        match self {
            // TODO(https://fxbug.dev/328306129) handle boot and monotonic time separately
            Self::BootTime | Self::Monotonic => zx::Time::get_monotonic(),
            Self::RealTime => utc::utc_now(),
        }
    }
}

pub enum TimerWakeup {
    /// A regular timer that does not wake the system if it is suspended.
    Regular,
    /// An alarm timer that will wake the system if it is suspended.
    Alarm,
}
