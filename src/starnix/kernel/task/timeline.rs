// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::time::utc;
use fuchsia_runtime::{UtcDuration, UtcInstant};
use starnix_types::time::{itimerspec_from_deadline_interval, time_from_timespec};
use starnix_uapi::errors::Errno;
use starnix_uapi::{itimerspec, timespec};
use std::ops;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Timeline {
    RealTime,
    Monotonic,
    BootInstant,
}

impl Timeline {
    /// Returns the current time on this timeline.
    pub fn now(&self) -> TargetTime {
        match self {
            Self::RealTime => TargetTime::RealTime(utc::utc_now()),
            Self::Monotonic => TargetTime::Monotonic(zx::MonotonicInstant::get()),
            Self::BootInstant => TargetTime::BootInstant(zx::BootInstant::get()),
        }
    }

    pub fn target_from_timespec(&self, spec: timespec) -> Result<TargetTime, Errno> {
        Ok(match self {
            Timeline::Monotonic => TargetTime::Monotonic(time_from_timespec(spec)?),
            Timeline::RealTime => TargetTime::RealTime(time_from_timespec(spec)?),
            Timeline::BootInstant => TargetTime::BootInstant(time_from_timespec(spec)?),
        })
    }

    pub fn zero_time(&self) -> TargetTime {
        match self {
            Timeline::Monotonic => TargetTime::Monotonic(zx::Instant::ZERO),
            Timeline::RealTime => TargetTime::RealTime(zx::Instant::ZERO),
            Timeline::BootInstant => TargetTime::BootInstant(zx::Instant::ZERO),
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
    Monotonic(zx::MonotonicInstant),
    RealTime(UtcInstant),
    BootInstant(zx::BootInstant),
}

impl TargetTime {
    pub fn is_zero(&self) -> bool {
        0 == match self {
            TargetTime::Monotonic(t) => t.into_nanos(),
            TargetTime::RealTime(t) => t.into_nanos(),
            TargetTime::BootInstant(t) => t.into_nanos(),
        }
    }

    pub fn itimerspec(&self, interval: zx::MonotonicDuration) -> itimerspec {
        match self {
            TargetTime::Monotonic(t) => itimerspec_from_deadline_interval(*t, interval),
            TargetTime::BootInstant(t) => itimerspec_from_deadline_interval(*t, interval),
            TargetTime::RealTime(t) => itimerspec_from_deadline_interval(*t, interval),
        }
    }

    pub fn estimate_boot(&self) -> Option<zx::BootInstant> {
        match self {
            TargetTime::BootInstant(t) => Some(*t),
            // TODO(https://fxbug.dev/369653367): estimate_boot_deadline_from_utc
            TargetTime::RealTime(t) => Some(zx::BootInstant::from_nanos(
                utc::estimate_monotonic_deadline_from_utc(*t).into_nanos(),
            )),
            // It's not possible to estimate how long suspensions will be.
            TargetTime::Monotonic(_) => None,
        }
    }

    /// Find the difference between this time and `rhs`. Returns `None` if the timelines don't
    /// match.
    pub fn delta(&self, rhs: &Self) -> Option<GenericDuration> {
        match (*self, *rhs) {
            (TargetTime::Monotonic(lhs), TargetTime::Monotonic(rhs)) => {
                Some(GenericDuration::from(lhs - rhs))
            }
            (TargetTime::BootInstant(lhs), TargetTime::BootInstant(rhs)) => {
                Some(GenericDuration::from(lhs - rhs))
            }
            (TargetTime::RealTime(lhs), TargetTime::RealTime(rhs)) => {
                Some(GenericDuration::from(lhs - rhs))
            }
            _ => None,
        }
    }
}

impl std::ops::Add<GenericDuration> for TargetTime {
    type Output = Self;
    fn add(self, rhs: GenericDuration) -> Self {
        match self {
            Self::RealTime(t) => Self::RealTime(t + rhs.into_utc()),
            Self::Monotonic(t) => Self::Monotonic(t + rhs.into_mono()),
            Self::BootInstant(t) => Self::BootInstant(t + rhs.into_boot()),
        }
    }
}

impl std::ops::Sub<GenericDuration> for TargetTime {
    type Output = Self;
    fn sub(self, rhs: GenericDuration) -> Self::Output {
        match self {
            TargetTime::Monotonic(t) => Self::Monotonic(t - rhs.into_mono()),
            TargetTime::RealTime(t) => Self::RealTime(t - rhs.into_utc()),
            TargetTime::BootInstant(t) => Self::BootInstant(t - rhs.into_boot()),
        }
    }
}

impl std::cmp::PartialOrd for TargetTime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Self::Monotonic(lhs), Self::Monotonic(rhs)) => Some(lhs.cmp(rhs)),
            (Self::RealTime(lhs), Self::RealTime(rhs)) => Some(lhs.cmp(rhs)),
            (Self::BootInstant(lhs), Self::BootInstant(rhs)) => Some(lhs.cmp(rhs)),
            _ => None,
        }
    }
}

/// This type is a convenience to use when the Timeline isn't clear in Starnix. It allows storing a
/// generic nanosecond duration which can be used to operate on Instants or Durations from any
/// Timeline.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct GenericDuration(zx::SyntheticDuration);

impl GenericDuration {
    pub fn from_nanos(nanos: zx::sys::zx_time_t) -> Self {
        Self(zx::Duration::from_nanos(nanos))
    }

    pub fn into_mono(self) -> zx::MonotonicDuration {
        zx::MonotonicDuration::from_nanos(self.0.into_nanos())
    }

    // TODO(https://fxbug.dev/328306129) handle boot and monotonic time properly and remove this
    // allow.
    #[allow(dead_code)]
    fn into_boot(self) -> zx::BootDuration {
        zx::BootDuration::from_nanos(self.0.into_nanos())
    }

    fn into_utc(self) -> UtcDuration {
        UtcDuration::from_nanos(self.0.into_nanos())
    }
}

impl From<zx::MonotonicDuration> for GenericDuration {
    fn from(other: zx::MonotonicDuration) -> Self {
        Self(zx::SyntheticDuration::from_nanos(other.into_nanos()))
    }
}

impl From<zx::BootDuration> for GenericDuration {
    fn from(other: zx::BootDuration) -> Self {
        Self(zx::SyntheticDuration::from_nanos(other.into_nanos()))
    }
}

impl From<zx::SyntheticDuration> for GenericDuration {
    fn from(other: zx::SyntheticDuration) -> Self {
        Self(other)
    }
}

impl From<UtcDuration> for GenericDuration {
    fn from(other: UtcDuration) -> Self {
        Self(zx::SyntheticDuration::from_nanos(other.into_nanos()))
    }
}

impl ops::Deref for GenericDuration {
    type Target = zx::SyntheticDuration;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
