// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Type-safe bindings for Zircon clock update objects.
//!
//! Example usage:
//! ```rust
//! clock.update(ClockUpdate::builder().approximate_value(updated_time)).expect("update failed");
//!
//! let update = ClockUpdate::builder().rate_adjust(42).error_bounds(1_000_000).build();
//! clock.update(update).expect("update failed");
//! ```

use crate::{MonotonicTime, SyntheticTimeline, Time, Timeline};
use fuchsia_zircon_sys as sys;
use std::fmt::Debug;

/// A trait implemented by all components of a ClockUpdateBuilder's state.
pub trait State {
    /// Records the validity in the supplied bitfield.
    fn add_options(&self, options: &mut u64);
}

/// A trait implemented by states that describe how to set a clock value.
pub trait ValueState: State {
    type OutputTimeline: Timeline;
    fn reference_value(&self) -> Option<MonotonicTime>;
    fn synthetic_value(&self) -> Option<Time<Self::OutputTimeline>>;
}

/// A trait implemented by states that describe how to set a clock rate.
pub trait RateState: State {
    fn rate_adjustment(&self) -> Option<i32>;
}

/// A trait implemented by states that describe how to set a clock error.
pub trait ErrorState: State {
    fn error_bound(&self) -> Option<u64>;
}

/// A `ClockUpdateBuilder` state indicating no change.
pub struct Null<O>(std::marker::PhantomData<O>);

impl<O: Timeline> State for Null<O> {
    fn add_options(&self, _: &mut u64) {}
}

impl<O: Timeline> ValueState for Null<O> {
    type OutputTimeline = O;
    fn reference_value(&self) -> Option<MonotonicTime> {
        None
    }
    fn synthetic_value(&self) -> Option<Time<O>> {
        None
    }
}
impl<O: Timeline> RateState for Null<O> {
    fn rate_adjustment(&self) -> Option<i32> {
        None
    }
}
impl<O: Timeline> ErrorState for Null<O> {
    fn error_bound(&self) -> Option<u64> {
        None
    }
}

/// A `ClockUpdateBuilder` state indicating value should be set using a
/// (reference time, synthetic time) tuple.
pub struct AbsoluteValue<O> {
    reference_value: MonotonicTime,
    synthetic_value: Time<O>,
    _output_marker: std::marker::PhantomData<O>,
}

impl<O: Timeline + Copy> State for AbsoluteValue<O> {
    #[inline]
    fn add_options(&self, opts: &mut u64) {
        *opts |= sys::ZX_CLOCK_UPDATE_OPTION_REFERENCE_VALUE_VALID
            | sys::ZX_CLOCK_UPDATE_OPTION_SYNTHETIC_VALUE_VALID;
    }
}

impl<O: Timeline + Copy> ValueState for AbsoluteValue<O> {
    type OutputTimeline = O;
    fn reference_value(&self) -> Option<MonotonicTime> {
        Some(self.reference_value)
    }
    fn synthetic_value(&self) -> Option<Time<O>> {
        Some(self.synthetic_value)
    }
}

/// A `ClockUpdateBuilder` state indicating value should be set using only a synthetic time.
pub struct ApproximateValue<O>(Time<O>);

impl<O: Timeline + Copy> State for ApproximateValue<O> {
    #[inline]
    fn add_options(&self, opts: &mut u64) {
        *opts |= sys::ZX_CLOCK_UPDATE_OPTION_SYNTHETIC_VALUE_VALID;
    }
}

impl<O: Timeline + Copy> ValueState for ApproximateValue<O> {
    type OutputTimeline = O;
    fn reference_value(&self) -> Option<MonotonicTime> {
        None
    }
    fn synthetic_value(&self) -> Option<Time<O>> {
        Some(self.0)
    }
}

/// A clock update state indicating the rate should be set using the contained ppm offset.
pub struct Rate(i32);

impl State for Rate {
    #[inline]
    fn add_options(&self, opts: &mut u64) {
        *opts |= sys::ZX_CLOCK_UPDATE_OPTION_RATE_ADJUST_VALID;
    }
}

impl RateState for Rate {
    fn rate_adjustment(&self) -> Option<i32> {
        Some(self.0)
    }
}

/// A clock update state indicating the clock error should be set using the contained bound in
/// nanoseconds.
pub struct Error(u64);

impl State for Error {
    #[inline]
    fn add_options(&self, opts: &mut u64) {
        *opts |= sys::ZX_CLOCK_UPDATE_OPTION_ERROR_BOUND_VALID;
    }
}

impl ErrorState for Error {
    fn error_bound(&self) -> Option<u64> {
        Some(self.0)
    }
}

/// Builder to specify how zero or more properties of a clock should be updated.
/// See [`Clock::update`].
///
/// A `ClockUpdateBuilder` may be created using `ClockUpdate::builder()`.
#[derive(Debug, Eq, PartialEq)]
pub struct ClockUpdateBuilder<V, R, E, O> {
    value_state: V,
    rate_state: R,
    error_state: E,
    _output_marker: std::marker::PhantomData<O>,
}

impl<V: ValueState<OutputTimeline = O>, R: RateState, E: ErrorState, O: Timeline>
    ClockUpdateBuilder<V, R, E, O>
{
    /// Converts this `ClockUpdateBuilder` to a `ClockUpdate`.
    #[inline]
    pub fn build(self) -> ClockUpdate<O> {
        ClockUpdate::from(self)
    }
}

impl<O: Timeline> ClockUpdateBuilder<Null<O>, Null<O>, Null<O>, O> {
    /// Returns an empty `ClockUpdateBuilder`.
    #[inline]
    fn new() -> Self {
        Self {
            value_state: Null(std::marker::PhantomData),
            rate_state: Null(std::marker::PhantomData),
            error_state: Null(std::marker::PhantomData),
            _output_marker: std::marker::PhantomData,
        }
    }
}

impl<R: RateState, E: ErrorState, O: Timeline> ClockUpdateBuilder<Null<O>, R, E, O> {
    /// Sets an absolute value for this `ClockUpdate` using a (reference time, synthetic time) pair.
    ///
    /// Reference time is typically monotonic and synthetic time is the time tracked by the clock.
    /// Adding an absolute value is only possible when no other value has been set.
    #[inline]
    pub fn absolute_value(
        self,
        reference_value: MonotonicTime,
        synthetic_value: Time<O>,
    ) -> ClockUpdateBuilder<AbsoluteValue<O>, R, E, O> {
        ClockUpdateBuilder {
            value_state: AbsoluteValue {
                reference_value,
                synthetic_value,
                _output_marker: std::marker::PhantomData,
            },
            rate_state: self.rate_state,
            error_state: self.error_state,
            _output_marker: std::marker::PhantomData,
        }
    }
}

impl<E: ErrorState, O: Timeline> ClockUpdateBuilder<Null<O>, Null<O>, E, O> {
    /// Sets an approximate value for this `ClockUpdateBuilder` using a synthetic time only.
    ///
    /// Synthetic time is the time tracked by the clock. The reference time will be set to current
    /// monotonic time when the kernel applies this clock update, meaning any delay between
    /// calculating synthetic time and applying the update will result in a clock error. Adding an
    /// approximate value is only possible when no other value has been set and when no rate has
    /// been set.
    #[inline]
    pub fn approximate_value(
        self,
        synthetic_value: Time<O>,
    ) -> ClockUpdateBuilder<ApproximateValue<O>, Null<O>, E, O> {
        ClockUpdateBuilder {
            value_state: ApproximateValue(synthetic_value),
            rate_state: self.rate_state,
            error_state: self.error_state,
            _output_marker: std::marker::PhantomData,
        }
    }
}

impl<E: ErrorState, O: Timeline> ClockUpdateBuilder<Null<O>, Null<O>, E, O> {
    /// Adds a rate change in parts per million to this `ClockUpdateBuilder`.
    ///
    /// Adding a rate is only possible when the value is either not set or set to an absolute value
    /// and when no rate has been set previously.
    #[inline]
    pub fn rate_adjust(self, rate_adjust_ppm: i32) -> ClockUpdateBuilder<Null<O>, Rate, E, O> {
        ClockUpdateBuilder {
            value_state: self.value_state,
            rate_state: Rate(rate_adjust_ppm),
            error_state: self.error_state,
            _output_marker: std::marker::PhantomData,
        }
    }
}

impl<E: ErrorState, O: Timeline> ClockUpdateBuilder<AbsoluteValue<O>, Null<O>, E, O> {
    /// Adds a rate change in parts per million to this `ClockUpdateBuilder`.
    ///
    /// Adding a rate is only possible when the value is either not set or set to an absolute value
    /// and when no rate has been set previously.
    #[inline]
    pub fn rate_adjust(
        self,
        rate_adjust_ppm: i32,
    ) -> ClockUpdateBuilder<AbsoluteValue<O>, Rate, E, O> {
        ClockUpdateBuilder {
            value_state: self.value_state,
            rate_state: Rate(rate_adjust_ppm),
            error_state: self.error_state,
            _output_marker: std::marker::PhantomData,
        }
    }
}

impl<V: ValueState, R: RateState, O: Timeline> ClockUpdateBuilder<V, R, Null<O>, O> {
    /// Adds an error bound in nanoseconds to this `ClockUpdateBuilder`.
    #[inline]
    pub fn error_bounds(self, error_bound_ns: u64) -> ClockUpdateBuilder<V, R, Error, O> {
        ClockUpdateBuilder {
            value_state: self.value_state,
            rate_state: self.rate_state,
            error_state: Error(error_bound_ns),
            _output_marker: std::marker::PhantomData,
        }
    }
}

/// Specifies an update to zero or more properties of a clock. See [`Clock::update`]
#[derive(Debug, Eq, PartialEq)]
pub struct ClockUpdate<Output = SyntheticTimeline> {
    options: u64,
    rate_adjust: i32,
    synthetic_value: Time<Output>,
    reference_value: MonotonicTime,
    error_bound: u64,
}

impl<O: Timeline> ClockUpdate<O> {
    /// Returns a new, empty, `ClockUpdateBuilder`.
    #[inline]
    pub fn builder() -> ClockUpdateBuilder<Null<O>, Null<O>, Null<O>, O> {
        ClockUpdateBuilder::new()
    }

    /// Returns a bitfield of options to pass to [`sys::zx_clock_update`] in conjunction with a
    /// `zx_clock_update_args_v2_t` generated from this `ClockUpdate`.
    #[inline]
    pub fn options(&self) -> u64 {
        self.options
    }
}

impl<V: ValueState<OutputTimeline = O>, R: RateState, E: ErrorState, O: Timeline>
    From<ClockUpdateBuilder<V, R, E, O>> for ClockUpdate<O>
{
    fn from(builder: ClockUpdateBuilder<V, R, E, O>) -> Self {
        let mut options = sys::ZX_CLOCK_ARGS_VERSION_2;
        builder.value_state.add_options(&mut options);
        builder.rate_state.add_options(&mut options);
        builder.error_state.add_options(&mut options);

        Self {
            options,
            rate_adjust: builder.rate_state.rate_adjustment().unwrap_or_default(),
            synthetic_value: builder.value_state.synthetic_value().unwrap_or_default(),
            reference_value: builder.value_state.reference_value().unwrap_or_default(),
            error_bound: builder.error_state.error_bound().unwrap_or_default(),
        }
    }
}

impl<Output: Timeline> From<ClockUpdate<Output>> for sys::zx_clock_update_args_v2_t {
    fn from(clock_update: ClockUpdate<Output>) -> Self {
        sys::zx_clock_update_args_v2_t {
            rate_adjust: clock_update.rate_adjust,
            padding1: Default::default(),
            synthetic_value: clock_update.synthetic_value.into_nanos(),
            reference_value: clock_update.reference_value.into_nanos(),
            error_bound: clock_update.error_bound,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SyntheticTime, SyntheticTimeline};

    #[test]
    fn empty_update() {
        let update = ClockUpdateBuilder::<_, _, _, SyntheticTimeline>::new().build();
        assert_eq!(update.options(), sys::ZX_CLOCK_ARGS_VERSION_2);
        assert_eq!(
            sys::zx_clock_update_args_v2_t::from(update),
            sys::zx_clock_update_args_v2_t {
                rate_adjust: 0,
                padding1: Default::default(),
                reference_value: 0,
                synthetic_value: 0,
                error_bound: 0,
            }
        );
    }

    #[test]
    fn rate_only() {
        let update =
            ClockUpdate::<SyntheticTimeline>::from(ClockUpdateBuilder::new().rate_adjust(52));
        assert_eq!(
            update.options(),
            sys::ZX_CLOCK_ARGS_VERSION_2 | sys::ZX_CLOCK_UPDATE_OPTION_RATE_ADJUST_VALID
        );
        assert_eq!(
            sys::zx_clock_update_args_v2_t::from(update),
            sys::zx_clock_update_args_v2_t {
                rate_adjust: 52,
                padding1: Default::default(),
                reference_value: 0,
                synthetic_value: 0,
                error_bound: 0,
            }
        );
    }

    #[test]
    fn approximate_value() {
        let update = ClockUpdateBuilder::new()
            .approximate_value(SyntheticTime::from_nanos(42))
            .error_bounds(62)
            .build();
        assert_eq!(
            update.options(),
            sys::ZX_CLOCK_ARGS_VERSION_2
                | sys::ZX_CLOCK_UPDATE_OPTION_SYNTHETIC_VALUE_VALID
                | sys::ZX_CLOCK_UPDATE_OPTION_ERROR_BOUND_VALID
        );
        assert_eq!(
            sys::zx_clock_update_args_v2_t::from(update),
            sys::zx_clock_update_args_v2_t {
                rate_adjust: 0,
                padding1: Default::default(),
                reference_value: 0,
                synthetic_value: 42,
                error_bound: 62,
            }
        );
    }

    #[test]
    fn absolute_value() {
        let update = ClockUpdateBuilder::new()
            .absolute_value(MonotonicTime::from_nanos(1000), SyntheticTime::from_nanos(42))
            .rate_adjust(52)
            .error_bounds(62)
            .build();
        assert_eq!(
            update.options(),
            sys::ZX_CLOCK_ARGS_VERSION_2
                | sys::ZX_CLOCK_UPDATE_OPTION_REFERENCE_VALUE_VALID
                | sys::ZX_CLOCK_UPDATE_OPTION_SYNTHETIC_VALUE_VALID
                | sys::ZX_CLOCK_UPDATE_OPTION_RATE_ADJUST_VALID
                | sys::ZX_CLOCK_UPDATE_OPTION_ERROR_BOUND_VALID
        );
        assert_eq!(
            sys::zx_clock_update_args_v2_t::from(update),
            sys::zx_clock_update_args_v2_t {
                rate_adjust: 52,
                padding1: Default::default(),
                reference_value: 1000,
                synthetic_value: 42,
                error_bound: 62,
            }
        );
    }
}
