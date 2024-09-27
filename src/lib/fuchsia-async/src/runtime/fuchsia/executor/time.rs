// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::common::EHandle;
use crate::runtime::DurationExt;
use fuchsia_zircon as zx;
use std::ops;

/// A time relative to the executor's clock.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Time(zx::MonotonicInstant);

pub use zx::Duration;

impl Time {
    /// Return the current time according to the global executor.
    ///
    /// This function requires that an executor has been set up.
    pub fn now() -> Self {
        EHandle::local().inner().now()
    }

    /// Compute a deadline for the time in the future that is the
    /// given `Duration` away. Similarly to `zx::MonotonicInstant::after`,
    /// saturates on overflow instead of wrapping around.
    ///
    /// This function requires that an executor has been set up.
    pub fn after(duration: zx::Duration) -> Self {
        Self::now() + duration
    }

    /// Convert from `zx::MonotonicInstant`.
    pub const fn from_zx(t: zx::MonotonicInstant) -> Self {
        Time(t)
    }

    /// Convert into `zx::MonotonicInstant`.
    pub const fn into_zx(self) -> zx::MonotonicInstant {
        self.0
    }

    /// Convert from nanoseconds.
    pub const fn from_nanos(nanos: i64) -> Self {
        Self::from_zx(zx::MonotonicInstant::from_nanos(nanos))
    }

    /// Convert to nanoseconds.
    pub const fn into_nanos(self) -> i64 {
        self.0.into_nanos()
    }

    /// The maximum time.
    pub const INFINITE: Time = Time(zx::MonotonicInstant::INFINITE);

    /// The minimum time.
    pub const INFINITE_PAST: Time = Time(zx::MonotonicInstant::INFINITE_PAST);
}

impl From<zx::MonotonicInstant> for Time {
    fn from(t: zx::MonotonicInstant) -> Time {
        Time(t)
    }
}

impl From<Time> for zx::MonotonicInstant {
    fn from(t: Time) -> zx::MonotonicInstant {
        t.0
    }
}

impl ops::Add<zx::Duration> for Time {
    type Output = Time;
    fn add(self, d: zx::Duration) -> Time {
        Time(self.0 + d)
    }
}

impl ops::Add<Time> for zx::Duration {
    type Output = Time;
    fn add(self, t: Time) -> Time {
        Time(self + t.0)
    }
}

impl ops::Sub<zx::Duration> for Time {
    type Output = Time;
    fn sub(self, d: zx::Duration) -> Time {
        Time(self.0 - d)
    }
}

impl ops::Sub<Time> for Time {
    type Output = zx::Duration;
    fn sub(self, t: Time) -> zx::Duration {
        self.0 - t.0
    }
}

impl ops::AddAssign<zx::Duration> for Time {
    fn add_assign(&mut self, d: zx::Duration) {
        self.0.add_assign(d)
    }
}

impl ops::SubAssign<zx::Duration> for Time {
    fn sub_assign(&mut self, d: zx::Duration) {
        self.0.sub_assign(d)
    }
}

impl DurationExt for zx::Duration {
    fn after_now(self) -> Time {
        Time::after(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuchsia_zircon::{self as zx};

    fn time_operations_param(
        zxt1: zx::MonotonicInstant,
        zxt2: zx::MonotonicInstant,
        d: zx::Duration,
    ) {
        let t1 = Time::from_zx(zxt1);
        let t2 = Time::from_zx(zxt2);
        assert_eq!(t1.into_zx(), zxt1);

        assert_eq!(Time::from_zx(zx::MonotonicInstant::INFINITE), Time::INFINITE);
        assert_eq!(Time::from_zx(zx::MonotonicInstant::INFINITE_PAST), Time::INFINITE_PAST);
        assert_eq!(zxt1 - zxt2, t1 - t2);
        assert_eq!(zxt1 + d, (t1 + d).into_zx());
        assert_eq!(d + zxt1, (d + t1).into_zx());
        assert_eq!(zxt1 - d, (t1 - d).into_zx());

        let mut zxt = zxt1;
        let mut t = t1;
        t += d;
        zxt += d;
        assert_eq!(zxt, t.into_zx());
        t -= d;
        zxt -= d;
        assert_eq!(zxt, t.into_zx());
    }

    #[test]
    fn time_operations() {
        time_operations_param(
            zx::MonotonicInstant::from_nanos(0),
            zx::MonotonicInstant::from_nanos(1000),
            zx::Duration::from_seconds(12),
        );
        time_operations_param(
            zx::MonotonicInstant::from_nanos(-100000),
            zx::MonotonicInstant::from_nanos(65324),
            zx::Duration::from_hours(-785),
        );
    }

    #[test]
    fn time_saturating_add() {
        assert_eq!(Time::from_nanos(10) + zx::Duration::from_nanos(30), Time::from_nanos(40));
        assert_eq!(
            Time::from_nanos(10) + zx::Duration::from_nanos(Time::INFINITE.into_nanos()),
            Time::INFINITE
        );
        assert_eq!(
            Time::from_nanos(-10) + zx::Duration::from_nanos(Time::INFINITE_PAST.into_nanos()),
            Time::INFINITE_PAST
        );
    }
}
