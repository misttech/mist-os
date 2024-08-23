// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::time::utc;
use fuchsia_runtime::UtcTime;
use fuchsia_zircon as zx;
use starnix_uapi::errors::Errno;
use starnix_uapi::time::{itimerspec_from_deadline_interval, time_from_timespec};
use starnix_uapi::{itimerspec, timespec};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Timeline {
    RealTime,
    Monotonic,
    BootTime,
}

impl Timeline {
    /// Returns the current time on this timeline.
    pub fn now(&self) -> TargetTime {
        match self {
            Self::RealTime => TargetTime::RealTime(utc::utc_now()),
            Self::Monotonic => TargetTime::Monotonic(zx::MonotonicTime::get()),
            // TODO(https://fxbug.dev/328306129) handle boot and monotonic time separately
            Self::BootTime => TargetTime::BootTime(zx::MonotonicTime::get()),
        }
    }

    pub fn target_from_timespec(&self, spec: timespec) -> Result<TargetTime, Errno> {
        Ok(match self {
            Timeline::Monotonic => TargetTime::Monotonic(time_from_timespec(spec)?),
            Timeline::RealTime => TargetTime::RealTime(time_from_timespec(spec)?),
            Timeline::BootTime => TargetTime::BootTime(time_from_timespec(spec)?),
        })
    }

    pub fn zero_time(&self) -> TargetTime {
        match self {
            Timeline::Monotonic => TargetTime::Monotonic(zx::Time::from_nanos(0)),
            Timeline::RealTime => TargetTime::RealTime(zx::Time::from_nanos(0)),
            Timeline::BootTime => TargetTime::BootTime(zx::Time::from_nanos(0)),
        }
    }
}

#[derive(Debug)]
pub enum TimerWakeup {
    /// A regular timer that does not wake the system if it is suspended.
    Regular,
    /// An alarm timer that will wake the system if it is suspended.
    Alarm,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TargetTime {
    Monotonic(zx::MonotonicTime),
    RealTime(UtcTime),
    // TODO(https://fxbug.dev/328306129) handle boot time with its own type
    BootTime(zx::MonotonicTime),
}

impl TargetTime {
    pub fn is_zero(&self) -> bool {
        0 == match self {
            TargetTime::Monotonic(t) => t.into_nanos(),
            TargetTime::RealTime(t) => t.into_nanos(),
            TargetTime::BootTime(t) => t.into_nanos(),
        }
    }

    pub fn itimerspec(&self, interval: zx::Duration) -> itimerspec {
        match self {
            TargetTime::Monotonic(t) | TargetTime::BootTime(t) => {
                itimerspec_from_deadline_interval(*t, interval)
            }
            TargetTime::RealTime(t) => itimerspec_from_deadline_interval(*t, interval),
        }
    }

    // TODO(https://fxbug.dev/328306129) handle boot and monotonic time properly
    pub fn estimate_monotonic(&self) -> zx::MonotonicTime {
        match self {
            TargetTime::BootTime(t) | TargetTime::Monotonic(t) => *t,
            TargetTime::RealTime(t) => utc::estimate_monotonic_deadline_from_utc(*t),
        }
    }

    /// Find the difference between this time and `rhs`. Returns `None` if the timelines don't
    /// match.
    pub fn delta(&self, rhs: &Self) -> Option<zx::Duration> {
        match (*self, *rhs) {
            (TargetTime::Monotonic(lhs), TargetTime::Monotonic(rhs)) => Some(lhs - rhs),
            (TargetTime::BootTime(lhs), TargetTime::BootTime(rhs)) => Some(lhs - rhs),
            (TargetTime::RealTime(lhs), TargetTime::RealTime(rhs)) => Some(lhs - rhs),
            _ => None,
        }
    }
}

impl std::ops::Add<zx::Duration> for TargetTime {
    type Output = Self;
    fn add(self, rhs: zx::Duration) -> Self {
        match self {
            Self::RealTime(t) => Self::RealTime(t + rhs),
            Self::Monotonic(t) => Self::Monotonic(t + rhs),
            Self::BootTime(t) => Self::BootTime(t + rhs),
        }
    }
}

impl std::ops::Sub<zx::Duration> for TargetTime {
    type Output = zx::Duration;
    fn sub(self, rhs: zx::Duration) -> Self::Output {
        match self {
            TargetTime::Monotonic(t) => zx::Duration::from_nanos((t - rhs).into_nanos()),
            TargetTime::RealTime(t) => zx::Duration::from_nanos((t - rhs).into_nanos()),
            TargetTime::BootTime(t) => zx::Duration::from_nanos((t - rhs).into_nanos()),
        }
    }
}

impl std::cmp::PartialOrd for TargetTime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Self::Monotonic(lhs), Self::Monotonic(rhs)) => Some(lhs.cmp(rhs)),
            (Self::RealTime(lhs), Self::RealTime(rhs)) => Some(lhs.cmp(rhs)),
            (Self::BootTime(lhs), Self::BootTime(rhs)) => Some(lhs.cmp(rhs)),
            _ => None,
        }
    }
}
