// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Test utilities for dealing with time.

use core::{
    fmt::{self, Debug, Formatter},
    ops,
    time::Duration,
};

use crate::time::{Instant, InstantBindingsTypes, InstantContext};

/// A fake implementation of `Instant` for use in testing.
#[derive(Default, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FakeInstant {
    /// A FakeInstant is just an offset from some arbitrary epoch.
    pub offset: Duration,
}

impl crate::inspect::InspectableValue for FakeInstant {
    fn record<I: crate::inspect::Inspector>(&self, _name: &str, _inspector: &mut I) {
        unimplemented!()
    }
}

impl FakeInstant {
    /// The maximum value represented by a fake instant.
    pub const LATEST: FakeInstant = FakeInstant { offset: Duration::MAX };

    /// Adds to this fake instant, saturating at [`LATEST`].
    pub fn saturating_add(self, dur: Duration) -> FakeInstant {
        FakeInstant { offset: self.offset.saturating_add(dur) }
    }
}

impl From<Duration> for FakeInstant {
    fn from(offset: Duration) -> FakeInstant {
        FakeInstant { offset }
    }
}

impl Instant for FakeInstant {
    fn duration_since(&self, earlier: FakeInstant) -> Duration {
        self.offset.checked_sub(earlier.offset).unwrap()
    }

    fn saturating_duration_since(&self, earlier: FakeInstant) -> Duration {
        self.offset.saturating_sub(earlier.offset)
    }

    fn checked_add(&self, duration: Duration) -> Option<FakeInstant> {
        self.offset.checked_add(duration).map(|offset| FakeInstant { offset })
    }

    fn checked_sub(&self, duration: Duration) -> Option<FakeInstant> {
        self.offset.checked_sub(duration).map(|offset| FakeInstant { offset })
    }
}

impl ops::Add<Duration> for FakeInstant {
    type Output = FakeInstant;

    fn add(self, dur: Duration) -> FakeInstant {
        FakeInstant { offset: self.offset + dur }
    }
}

impl ops::Sub<FakeInstant> for FakeInstant {
    type Output = Duration;

    fn sub(self, other: FakeInstant) -> Duration {
        self.offset - other.offset
    }
}

impl ops::Sub<Duration> for FakeInstant {
    type Output = FakeInstant;

    fn sub(self, dur: Duration) -> FakeInstant {
        FakeInstant { offset: self.offset - dur }
    }
}

impl Debug for FakeInstant {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.offset)
    }
}

/// A fake [`InstantContext`] which stores the current time as a
/// [`FakeInstant`].
#[derive(Default)]
pub struct FakeInstantCtx {
    /// The fake instant held by this fake context.
    pub time: FakeInstant,
}

impl FakeInstantCtx {
    /// Advance the current time by the given duration.
    pub fn sleep(&mut self, dur: Duration) {
        self.time.offset += dur;
    }
}

impl InstantBindingsTypes for FakeInstantCtx {
    type Instant = FakeInstant;
}

impl InstantContext for FakeInstantCtx {
    fn now(&self) -> FakeInstant {
        self.time
    }
}

impl<T: AsRef<FakeInstantCtx>> InstantBindingsTypes for T {
    type Instant = FakeInstant;
}

impl<T: AsRef<FakeInstantCtx>> InstantContext for T {
    fn now(&self) -> FakeInstant {
        self.as_ref().now()
    }
}
