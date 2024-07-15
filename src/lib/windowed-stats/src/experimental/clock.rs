// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This module provides a thin abstraction over chronological types using type definitions. This
// can be further abstracted through dedicated types and more sophisticated APIs that support
// different clock implementations. At time of writing, it is tightly integrated with
// `fuchsia_async` and quantizes time to 1s.

//! Monotonic clock and chronological APIs.

use num::Integer;
use std::fmt::Display;
use std::mem;
use thiserror::Error;

/// Monotonic time error.
///
/// Describes temporal errors that occur when points in time are unexpectedly less (earlier) than
/// some reference time.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("non-monotonic tick or timestamp")]
pub struct MonotonicityError;

/// Unit of time.
///
/// Time is expressed and quantized as integer multiples of this basis unit.
pub type Quanta = i64;

pub trait QuantaExt {
    /// Converts the quanta into a `Display` that describes a duration in the nearest (largest)
    /// units.
    fn into_nearest_unit_display(self) -> impl Display;
}

impl QuantaExt for Quanta {
    fn into_nearest_unit_display(self) -> impl Display {
        const SECONDS_PER_MINUTE: i64 = 60;
        const SECONDS_PER_HOUR: i64 = SECONDS_PER_MINUTE * 60;
        const SECONDS_PER_DAY: i64 = SECONDS_PER_HOUR * 24;

        match (
            self.div_rem(&SECONDS_PER_DAY),
            self.div_rem(&SECONDS_PER_HOUR),
            self.div_rem(&SECONDS_PER_MINUTE),
        ) {
            ((days, 0), _, _) => format!("{}d", days),
            (_, (hours, 0), _) => format!("{}h", hours),
            (_, _, (minutes, 0)) => format!("{}m", minutes),
            _ => format!("{}s", self),
        }
    }
}

/// A point in time.
pub type Timestamp = fuchsia_async::Time;

pub trait TimestampExt {
    fn quantize(self, modulus: Quanta) -> Quanta;
}

impl TimestampExt for Timestamp {
    /// Quantizes the timestamp by the given modulus.
    fn quantize(self, modulus: Quanta) -> Quanta {
        (self - Timestamp::from_nanos(0)).into_quanta() % modulus
    }
}

/// A vector in time.
pub type Duration = fuchsia_async::Duration;

pub trait DurationExt {
    /// The unit duration.
    ///
    /// Durations are expressed in terms of this unit and so cannot express periods beyond this
    /// resolution.
    const QUANTUM: Self;

    /// Constructs a `Duration` from quanta.
    fn from_quanta(quanta: Quanta) -> Self;

    /// Converts the duration into quanta.
    fn into_quanta(self) -> Quanta;
}

impl DurationExt for Duration {
    const QUANTUM: Self = Duration::from_seconds(1);

    fn from_quanta(quanta: Quanta) -> Self {
        Duration::from_seconds(quanta)
    }

    fn into_quanta(self) -> Quanta {
        self.into_seconds()
    }
}

/// An arrow in time.
///
/// A `Tick` represents a directed point or relative displacement in time with respect to some
/// reference time.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Tick {
    pub timestamp: Timestamp,
    pub duration: Duration,
}

impl Tick {
    /// Quantizes the tick by the given modulus into start and end points in time, in that order.
    pub fn quantize(self, modulus: Quanta) -> (Quanta, Quanta) {
        let start = self.timestamp.quantize(modulus);
        let end = (self.timestamp + self.duration).quantize(modulus);
        (start, end)
    }
}

/// A monotonic reference point in time that advances when a sample is observed.
#[derive(Clone, Copy, Debug)]
pub struct ObservationTime {
    last_sample_timestamp: Timestamp,
}

impl ObservationTime {
    /// Advances the observation time to the given point in time.
    ///
    /// Returns a [`Tick`] that describes the displacement in time.
    ///
    /// # Errors
    ///
    /// Returns an error if the given point in time is not monotonic with respect to the current
    /// observation time.
    ///
    /// [`Tick`]: crate::experimental::clock::Tick
    pub fn tick(&mut self, timestamp: Timestamp) -> Result<Tick, MonotonicityError> {
        if timestamp < self.last_sample_timestamp {
            Err(MonotonicityError)
        } else {
            let duration = timestamp - self.last_sample_timestamp;
            let timestamp = mem::replace(&mut self.last_sample_timestamp, timestamp);
            Ok(Tick { timestamp, duration })
        }
    }
}

impl Default for ObservationTime {
    fn default() -> Self {
        ObservationTime { last_sample_timestamp: Timestamp::now() }
    }
}

/// A sample associated with a [point in time][`Timestamp`].
///
/// [`Timestamp`]: crate::experimental::clock::Timestamp
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TimedSample<T> {
    timestamp: Timestamp,
    sample: T,
}

impl<T> TimedSample<T> {
    /// Constructs a `Timed` from a sample at the [current time][`Timestamp::now`].
    ///
    /// [`Timestamp::now`]: crate::experimental::clock::Timestamp::now
    pub fn now(sample: T) -> Self {
        TimedSample { timestamp: Timestamp::now(), sample }
    }

    /// Constructs a `Timed` from a sample at the given point in time.
    pub fn at(sample: T, timestamp: impl Into<Timestamp>) -> Self {
        TimedSample { timestamp: timestamp.into(), sample }
    }
}

impl<T> From<TimedSample<T>> for (Timestamp, T) {
    fn from(timed: TimedSample<T>) -> Self {
        let TimedSample { timestamp, sample } = timed;
        (timestamp, sample)
    }
}

#[cfg(test)]
mod tests {
    use crate::experimental::clock::{
        Duration, DurationExt as _, MonotonicityError, ObservationTime, QuantaExt as _, Tick,
        TimedSample, Timestamp, TimestampExt as _,
    };
    use fuchsia_async as fasync;

    #[test]
    fn quantize() {
        let timestamp = Timestamp::from_nanos(0) + Duration::QUANTUM;
        assert_eq!(timestamp.quantize(1), 0);
        assert_eq!(timestamp.quantize(2), 1);

        let tick = Tick { timestamp, duration: Duration::from_quanta(3) };
        assert_eq!(tick.quantize(1), (0, 0));
        assert_eq!(tick.quantize(3), (1, 1));
    }

    #[test]
    fn tick() {
        let timestamp = Timestamp::from_nanos(0);
        let mut last = ObservationTime { last_sample_timestamp: timestamp };

        let tick = last.tick(timestamp + Duration::from_minutes(1)).unwrap();
        assert_eq!(tick, Tick { timestamp, duration: Duration::from_minutes(1) });

        let tick = last.tick(timestamp + Duration::from_minutes(2)).unwrap();
        assert_eq!(
            tick,
            Tick {
                timestamp: timestamp + Duration::from_minutes(1),
                duration: Duration::from_minutes(1)
            },
        );

        let result = last.tick(timestamp + Duration::from_minutes(1));
        assert_eq!(result, Err(MonotonicityError));
    }

    #[test]
    fn fmt_display_quanta() {
        assert_eq!(0i64.into_nearest_unit_display().to_string(), "0d");
        assert_eq!(1i64.into_nearest_unit_display().to_string(), "1s");
        assert_eq!(5i64.into_nearest_unit_display().to_string(), "5s");
        assert_eq!(60i64.into_nearest_unit_display().to_string(), "1m");
        assert_eq!(65i64.into_nearest_unit_display().to_string(), "65s");
        assert_eq!(120i64.into_nearest_unit_display().to_string(), "2m");
        assert_eq!(3600i64.into_nearest_unit_display().to_string(), "1h");
        assert_eq!(3605i64.into_nearest_unit_display().to_string(), "3605s");
        assert_eq!(86400i64.into_nearest_unit_display().to_string(), "1d");
    }

    #[test]
    fn timed_sample_now() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(3_000_000_000));
        let timed_sample = TimedSample::now(1u64);
        let (timestamp, sample) = timed_sample.into();
        assert_eq!(timestamp, Timestamp::from_nanos(3_000_000_000));
        assert_eq!(sample, 1u64);
    }

    #[test]
    fn timed_sample_at() {
        let timed_sample = TimedSample::at(1u64, Timestamp::from_nanos(3_000_000_000));
        let (timestamp, sample) = timed_sample.into();
        assert_eq!(timestamp, Timestamp::from_nanos(3_000_000_000));
        assert_eq!(sample, 1u64);
    }
}
