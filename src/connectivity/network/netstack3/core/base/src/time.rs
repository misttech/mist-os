// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common time abstractions.

pub(crate) mod local_timer_heap;
pub(crate) mod testutil;

use core::{fmt::Debug, time::Duration};

use crate::inspect::InspectableValue;

/// A type representing an instant in time.
///
/// `Instant` can be implemented by any type which represents an instant in
/// time. This can include any sort of real-world clock time (e.g.,
/// [`std::time::Instant`]) or fake time such as in testing.
pub trait Instant:
    Sized + Ord + Copy + Clone + Debug + Send + Sync + InspectableValue + 'static
{
    /// Returns the amount of time elapsed from another instant to this one.
    ///
    /// # Panics
    ///
    /// This function will panic if `earlier` is later than `self`.
    fn duration_since(&self, earlier: Self) -> core::time::Duration;

    /// Returns the amount of time elapsed from another instant to this one,
    /// saturating at zero.
    fn saturating_duration_since(&self, earlier: Self) -> core::time::Duration;

    /// Returns `Some(t)` where `t` is the time `self + duration` if `t` can be
    /// represented as `Instant` (which means it's inside the bounds of the
    /// underlying data structure), `None` otherwise.
    fn checked_add(&self, duration: core::time::Duration) -> Option<Self>;

    /// Unwraps the result from `checked_add`.
    ///
    /// # Panics
    ///
    /// This function will panic if the addition makes the clock wrap around.
    fn add(&self, duration: core::time::Duration) -> Self {
        self.checked_add(duration).unwrap_or_else(|| {
            panic!("clock wraps around when adding {:?} to {:?}", duration, *self);
        })
    }

    /// Returns `Some(t)` where `t` is the time `self - duration` if `t` can be
    /// represented as `Instant` (which means it's inside the bounds of the
    /// underlying data structure), `None` otherwise.
    fn checked_sub(&self, duration: core::time::Duration) -> Option<Self>;
}

/// Trait defining the `Instant` type provided by bindings' [`InstantContext`]
/// implementation.
///
/// It is a separate trait from `InstantContext` so the type stands by itself to
/// be stored at rest in core structures.
pub trait InstantBindingsTypes {
    /// The type of an instant in time.
    ///
    /// All time is measured using `Instant`s, including scheduling timers
    /// through [`TimerContext`]. This type may represent some sort of
    /// real-world time (e.g., [`std::time::Instant`]), or may be faked in
    /// testing using a fake clock.
    type Instant: Instant + 'static;
}

/// A context that provides access to a monotonic clock.
pub trait InstantContext: InstantBindingsTypes {
    /// Returns the current instant.
    ///
    /// `now` guarantees that two subsequent calls to `now` will return
    /// monotonically non-decreasing values.
    fn now(&self) -> Self::Instant;
}

/// Opaque types provided by bindings used by [`TimerContext`].
pub trait TimerBindingsTypes {
    /// State for a timer created through [`TimerContext`].
    type Timer: Debug + Send + Sync;
    /// The type used to dispatch fired timers from bindings to core.
    type DispatchId: Clone;
}

/// A context providing time scheduling to core.
// TODO(https://fxbug.dev/42083407): Remove '2' qualifiers when we delete the
// old trait. Note that all the methods that conflict with the old trait names
// have a disambiguating qualifier to make transitioning smoother.
pub trait TimerContext2: InstantContext + TimerBindingsTypes {
    /// Creates a new timer that dispatches `id` back to core when fired.
    ///
    /// Creating a new timer is an expensive operation and should be used
    /// sparingly. Modules should prefer to create a timer on creation and then
    /// schedule/reschedule it as needed. For modules with very dynamic timers,
    /// a [`LocalTimerHeap`] tied to a larger `Timer` might be a better
    /// alternative than creating many timers.
    fn new_timer(&mut self, id: Self::DispatchId) -> Self::Timer;

    /// Schedule a timer to fire at some point in the future.
    /// Returns the previously scheduled instant, if this timer was scheduled.
    fn schedule_timer_instant2(
        &mut self,
        time: Self::Instant,
        timer: &mut Self::Timer,
    ) -> Option<Self::Instant>;

    /// Like [`schedule_timer_instant2`] but schedules a time for `duration` in
    /// the future.
    fn schedule_timer2(
        &mut self,
        duration: Duration,
        timer: &mut Self::Timer,
    ) -> Option<Self::Instant> {
        self.schedule_timer_instant2(self.now().checked_add(duration).unwrap(), timer)
    }

    /// Cancel a timer.
    ///
    /// Cancels `timer`, returning the instant it was scheduled for if it was
    /// scheduled.
    ///
    /// Note that there's no guarantee that observing `None` means that the
    /// dispatch procedure for a previously fired timer has already concluded.
    /// It is possible to observe `None` here while the `DispatchId` `timer`
    /// was created with is still making its way to the module that originally
    /// scheduled this timer. If `Some` is observed, however, then the
    /// `TimerContext` guarantees this `timer` will *not* fire until
    ///[`schedule_timer_instant2`] is called to reschedule it.
    fn cancel_timer2(&mut self, timer: &mut Self::Timer) -> Option<Self::Instant>;

    /// Get the instant a timer will fire, if one is scheduled.
    fn scheduled_instant2(&self, timer: &mut Self::Timer) -> Option<Self::Instant>;
}

/// A core context providing timer type conversion.
///
/// This trait is used to convert from a core-internal timer type `T` to the
/// timer dispatch ID supported by bindings in `BT::DispatchId`.
pub trait CoreTimerContext<T, BT: TimerBindingsTypes> {
    /// Converts an inner timer to the bindings timer type.
    fn convert_timer(dispatch_id: T) -> BT::DispatchId;

    /// A helper function to create a new timer with the provided dispatch id.
    fn new_timer(bindings_ctx: &mut BT, dispatch_id: T) -> BT::Timer
    where
        BT: TimerContext2,
    {
        bindings_ctx.new_timer(Self::convert_timer(dispatch_id))
    }
}
