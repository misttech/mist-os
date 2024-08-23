// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Type-safe bindings for Zircon timer objects.

use crate::{ok, AsHandleRef, Handle, HandleBased, HandleRef, Status};
use fuchsia_zircon_sys as sys;
use std::cmp::{Eq, Ord, PartialEq, PartialOrd};
use std::hash::{Hash, Hasher};
use std::{ops, time as stdtime};

pub type MonotonicTime = Time<MonotonicTimeline>;
pub type SyntheticTime = Time<SyntheticTimeline>;

#[derive(Copy, Clone)]
#[repr(transparent)]
pub struct Time<T>(sys::zx_time_t, std::marker::PhantomData<T>);

impl<T> Default for Time<T> {
    fn default() -> Self {
        Time(0, std::marker::PhantomData)
    }
}

impl<T> Hash for Time<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl<T> PartialEq for Time<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<T> Eq for Time<T> {}

impl<T> PartialOrd for Time<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.0.partial_cmp(&other.0)
    }
}
impl<T> Ord for Time<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl<T> std::fmt::Debug for Time<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Avoid line noise from the marker type but do include the timeline in the output.
        let timeline_name = std::any::type_name::<T>();
        let short_timeline_name =
            timeline_name.rsplit_once("::").map(|(_, n)| n).unwrap_or(timeline_name);
        f.debug_tuple(&format!("Time<{short_timeline_name}>")).field(&self.0).finish()
    }
}

impl MonotonicTime {
    /// Get the current monotonic time.
    ///
    /// Wraps the
    /// [zx_clock_get_monotonic](https://fuchsia.dev/fuchsia-src/reference/syscalls/clock_get_monotonic.md)
    /// syscall.
    pub fn get() -> Self {
        unsafe { Self::from_nanos(sys::zx_clock_get_monotonic()) }
    }

    /// Compute a deadline for the time in the future that is the given `Duration` away.
    ///
    /// Wraps the
    /// [zx_deadline_after](https://fuchsia.dev/fuchsia-src/reference/syscalls/deadline_after.md)
    /// syscall.
    pub fn after(duration: Duration) -> Self {
        unsafe { Self::from_nanos(sys::zx_deadline_after(duration.0)) }
    }

    /// Sleep until the given time.
    ///
    /// Wraps the
    /// [zx_nanosleep](https://fuchsia.dev/fuchsia-src/reference/syscalls/nanosleep.md)
    /// syscall.
    pub fn sleep(self) {
        unsafe {
            sys::zx_nanosleep(self.0);
        }
    }
}

impl<T: Timeline> Time<T> {
    pub const INFINITE: Time<T> = Time(sys::ZX_TIME_INFINITE, std::marker::PhantomData);
    pub const INFINITE_PAST: Time<T> = Time(sys::ZX_TIME_INFINITE_PAST, std::marker::PhantomData);
    pub const ZERO: Time<T> = Time(0, std::marker::PhantomData);

    /// Returns the number of nanoseconds since the epoch contained by this `Time`.
    pub const fn into_nanos(self) -> i64 {
        self.0
    }

    pub const fn from_nanos(nanos: i64) -> Self {
        Time(nanos, std::marker::PhantomData)
    }
}

impl<T: Timeline> ops::Add<Duration> for Time<T> {
    type Output = Time<T>;
    fn add(self, dur: Duration) -> Self::Output {
        Time::from_nanos(dur.into_nanos().saturating_add(self.into_nanos()))
    }
}

impl<T: Timeline> ops::Sub<Duration> for Time<T> {
    type Output = Time<T>;
    fn sub(self, dur: Duration) -> Self::Output {
        Time::from_nanos(self.into_nanos().saturating_sub(dur.into_nanos()))
    }
}

impl<T: Timeline> ops::Sub<Time<T>> for Time<T> {
    type Output = Duration;
    fn sub(self, other: Time<T>) -> Self::Output {
        Duration::from_nanos(self.into_nanos().saturating_sub(other.into_nanos()))
    }
}

impl<T: Timeline> ops::AddAssign<Duration> for Time<T> {
    fn add_assign(&mut self, dur: Duration) {
        self.0 = self.0.saturating_add(dur.into_nanos());
    }
}

impl<T: Timeline> ops::SubAssign<Duration> for Time<T> {
    fn sub_assign(&mut self, dur: Duration) {
        self.0 = self.0.saturating_sub(dur.into_nanos());
    }
}

/// A marker trait for times to prevent accidental comparison between different timelines.
pub trait Timeline {}

#[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct MonotonicTimeline;
impl Timeline for MonotonicTimeline {}

#[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SyntheticTimeline;
impl Timeline for SyntheticTimeline {}

#[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Duration(sys::zx_duration_t);

impl From<stdtime::Duration> for Duration {
    fn from(dur: stdtime::Duration) -> Self {
        Duration::from_seconds(dur.as_secs() as i64)
            + Duration::from_nanos(dur.subsec_nanos() as i64)
    }
}

impl<T: Timeline> ops::Add<Time<T>> for Duration {
    type Output = Time<T>;
    fn add(self, time: Time<T>) -> Self::Output {
        Time::from_nanos(self.into_nanos().saturating_add(time.into_nanos()))
    }
}

impl ops::Add for Duration {
    type Output = Duration;
    fn add(self, dur: Duration) -> Duration {
        Duration::from_nanos(self.into_nanos().saturating_add(dur.into_nanos()))
    }
}

impl ops::Sub for Duration {
    type Output = Duration;
    fn sub(self, dur: Duration) -> Duration {
        Duration::from_nanos(self.into_nanos().saturating_sub(dur.into_nanos()))
    }
}

impl ops::AddAssign for Duration {
    fn add_assign(&mut self, dur: Duration) {
        self.0 = self.0.saturating_add(dur.into_nanos());
    }
}

impl ops::SubAssign for Duration {
    fn sub_assign(&mut self, dur: Duration) {
        self.0 = self.0.saturating_sub(dur.into_nanos());
    }
}

impl<T> ops::Mul<T> for Duration
where
    T: Into<i64>,
{
    type Output = Self;
    fn mul(self, mul: T) -> Self {
        Duration::from_nanos(self.0.saturating_mul(mul.into()))
    }
}

impl<T> ops::Div<T> for Duration
where
    T: Into<i64>,
{
    type Output = Self;
    fn div(self, div: T) -> Self {
        Duration::from_nanos(self.0.saturating_div(div.into()))
    }
}

impl ops::Neg for Duration {
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self(self.0.saturating_neg())
    }
}

impl Duration {
    pub const INFINITE: Duration = Duration(sys::zx_duration_t::MAX);
    pub const INFINITE_PAST: Duration = Duration(sys::zx_duration_t::MIN);
    pub const ZERO: Duration = Duration(0);

    /// Sleep for the given amount of time.
    pub fn sleep(self) {
        Time::after(self).sleep()
    }

    /// Returns the number of nanoseconds contained by this `Duration`.
    pub const fn into_nanos(self) -> i64 {
        self.0
    }

    /// Returns the total number of whole microseconds contained by this `Duration`.
    pub const fn into_micros(self) -> i64 {
        self.0 / 1_000
    }

    /// Returns the total number of whole milliseconds contained by this `Duration`.
    pub const fn into_millis(self) -> i64 {
        self.into_micros() / 1_000
    }

    /// Returns the total number of whole seconds contained by this `Duration`.
    pub const fn into_seconds(self) -> i64 {
        self.into_millis() / 1_000
    }

    /// Returns the duration as a floating-point value in seconds.
    pub fn into_seconds_f64(self) -> f64 {
        self.into_nanos() as f64 / 1_000_000_000f64
    }

    /// Returns the total number of whole minutes contained by this `Duration`.
    pub const fn into_minutes(self) -> i64 {
        self.into_seconds() / 60
    }

    /// Returns the total number of whole hours contained by this `Duration`.
    pub const fn into_hours(self) -> i64 {
        self.into_minutes() / 60
    }

    pub const fn from_nanos(nanos: i64) -> Self {
        Duration(nanos)
    }

    pub const fn from_micros(micros: i64) -> Self {
        Duration(micros.saturating_mul(1_000))
    }

    pub const fn from_millis(millis: i64) -> Self {
        Duration::from_micros(millis.saturating_mul(1_000))
    }

    pub const fn from_seconds(secs: i64) -> Self {
        Duration::from_millis(secs.saturating_mul(1_000))
    }

    pub const fn from_minutes(min: i64) -> Self {
        Duration::from_seconds(min.saturating_mul(60))
    }

    pub const fn from_hours(hours: i64) -> Self {
        Duration::from_minutes(hours.saturating_mul(60))
    }
}

pub trait DurationNum: Sized {
    fn nanos(self) -> Duration;
    fn micros(self) -> Duration;
    fn millis(self) -> Duration;
    fn seconds(self) -> Duration;
    fn minutes(self) -> Duration;
    fn hours(self) -> Duration;

    // Singular versions to allow for `1.milli()` and `1.second()`, etc.
    fn micro(self) -> Duration {
        self.micros()
    }
    fn milli(self) -> Duration {
        self.millis()
    }
    fn second(self) -> Duration {
        self.seconds()
    }
    fn minute(self) -> Duration {
        self.minutes()
    }
    fn hour(self) -> Duration {
        self.hours()
    }
}

// Note: this could be implemented for other unsized integer types, but it doesn't seem
// necessary to support the usual case.
impl DurationNum for i64 {
    fn nanos(self) -> Duration {
        Duration::from_nanos(self)
    }

    fn micros(self) -> Duration {
        Duration::from_micros(self)
    }

    fn millis(self) -> Duration {
        Duration::from_millis(self)
    }

    fn seconds(self) -> Duration {
        Duration::from_seconds(self)
    }

    fn minutes(self) -> Duration {
        Duration::from_minutes(self)
    }

    fn hours(self) -> Duration {
        Duration::from_hours(self)
    }
}

/// Read the number of high-precision timer ticks since boot. These ticks may be processor cycles,
/// high speed timer, profiling timer, etc. They are not guaranteed to continue advancing when the
/// system is asleep.
///
/// Wraps the
/// [zx_ticks_get](https://fuchsia.dev/fuchsia-src/reference/syscalls/ticks_get.md)
/// syscall.
pub fn ticks_get() -> i64 {
    unsafe { sys::zx_ticks_get() }
}

/// Return the number of high-precision timer ticks in a second.
///
/// Wraps the
/// [zx_ticks_per_second](https://fuchsia.dev/fuchsia-src/reference/syscalls/ticks_per_second.md)
/// syscall.
pub fn ticks_per_second() -> i64 {
    unsafe { sys::zx_ticks_per_second() }
}

/// An object representing a Zircon timer, such as the one returned by
/// [zx_timer_create](https://fuchsia.dev/fuchsia-src/reference/syscalls/timer_create.md).
///
/// As essentially a subtype of `Handle`, it can be freely interconverted.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Timer(Handle);
impl_handle_based!(Timer);

impl Timer {
    /// Create a timer, an object that can signal when a specified point in time has been reached.
    /// Wraps the
    /// [zx_timer_create](https://fuchsia.dev/fuchsia-src/reference/syscalls/timer_create.md)
    /// syscall.
    ///
    /// # Panics
    ///
    /// If the kernel reports no memory available to create a timer or the process' job policy
    /// denies timer creation.
    pub fn create() -> Self {
        let mut out = 0;
        let opts = 0;
        let status = unsafe {
            sys::zx_timer_create(opts, 0 /*ZX_CLOCK_MONOTONIC*/, &mut out)
        };
        ok(status)
            .expect("timer creation always succeeds except with OOM or when job policy denies it");
        unsafe { Self::from(Handle::from_raw(out)) }
    }

    /// Start a one-shot timer that will fire when `deadline` passes. Wraps the
    /// [zx_timer_set](https://fuchsia.dev/fuchsia-src/reference/syscalls/timer_set.md)
    /// syscall.
    pub fn set(&self, deadline: MonotonicTime, slack: Duration) -> Result<(), Status> {
        let status = unsafe {
            sys::zx_timer_set(self.raw_handle(), deadline.into_nanos(), slack.into_nanos())
        };
        ok(status)
    }

    /// Cancels a pending timer that was started with start(). Wraps the
    /// [zx_timer_cancel](https://fuchsia.dev/fuchsia-src/reference/syscalls/timer_cancel.md)
    /// syscall.
    pub fn cancel(&self) -> Result<(), Status> {
        let status = unsafe { sys::zx_timer_cancel(self.raw_handle()) };
        ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Signals;

    #[test]
    fn time_debug_repr_is_short() {
        assert_eq!(format!("{:?}", MonotonicTime::from_nanos(0)), "Time<MonotonicTimeline>(0)");
        assert_eq!(format!("{:?}", SyntheticTime::from_nanos(0)), "Time<SyntheticTimeline>(0)");
    }

    #[test]
    fn monotonic_time_increases() {
        let time1 = MonotonicTime::get();
        1_000.nanos().sleep();
        let time2 = MonotonicTime::get();
        assert!(time2 > time1);
    }

    #[test]
    fn ticks_increases() {
        let ticks1 = ticks_get();
        1_000.nanos().sleep();
        let ticks2 = ticks_get();
        assert!(ticks2 > ticks1);
    }

    #[test]
    fn tick_length() {
        let sleep_time = 1.milli();
        let ticks1 = ticks_get();
        sleep_time.sleep();
        let ticks2 = ticks_get();

        // The number of ticks should have increased by at least 1 ms worth
        let sleep_ticks = (sleep_time.into_millis() as i64) * ticks_per_second() / 1000;
        assert!(ticks2 >= (ticks1 + sleep_ticks));
    }

    #[test]
    fn sleep() {
        let sleep_ns = 1.millis();
        let time1 = MonotonicTime::get();
        sleep_ns.sleep();
        let time2 = MonotonicTime::get();
        assert!(time2 > time1 + sleep_ns);
    }

    #[test]
    fn from_std() {
        let std_dur = stdtime::Duration::new(25, 25);
        let dur = Duration::from(std_dur);
        let std_dur_nanos = (1_000_000_000 * std_dur.as_secs()) + std_dur.subsec_nanos() as u64;
        assert_eq!(std_dur_nanos as i64, dur.into_nanos());
    }

    #[test]
    fn i64_conversions() {
        let nanos_in_one_hour = 3_600_000_000_000;
        let dur_from_nanos = Duration::from_nanos(nanos_in_one_hour);
        let dur_from_hours = Duration::from_hours(1);
        assert_eq!(dur_from_nanos, dur_from_hours);
        assert_eq!(dur_from_nanos.into_nanos(), dur_from_hours.into_nanos());
        assert_eq!(dur_from_nanos.into_nanos(), nanos_in_one_hour);
        assert_eq!(dur_from_nanos.into_hours(), 1);
    }

    #[test]
    fn timer_basic() {
        let slack = 0.millis();
        let ten_ms = 10.millis();
        let five_secs = 5.seconds();

        // Create a timer
        let timer = Timer::create();

        // Should not signal yet.
        assert_eq!(
            timer.wait_handle(Signals::TIMER_SIGNALED, Time::after(ten_ms)),
            Err(Status::TIMED_OUT)
        );

        // Set it, and soon it should signal.
        assert_eq!(timer.set(Time::after(five_secs), slack), Ok(()));
        assert_eq!(
            timer.wait_handle(Signals::TIMER_SIGNALED, Time::INFINITE),
            Ok(Signals::TIMER_SIGNALED)
        );

        // Cancel it, and it should stop signalling.
        assert_eq!(timer.cancel(), Ok(()));
        assert_eq!(
            timer.wait_handle(Signals::TIMER_SIGNALED, Time::after(ten_ms)),
            Err(Status::TIMED_OUT)
        );
    }

    #[test]
    fn time_minus_time() {
        let lhs = MonotonicTime::from_nanos(10);
        let rhs = MonotonicTime::from_nanos(30);
        assert_eq!(lhs - rhs, Duration::from_nanos(-20));
    }

    #[test]
    fn time_saturation() {
        // Addition
        assert_eq!(
            MonotonicTime::from_nanos(10) + Duration::from_nanos(30),
            MonotonicTime::from_nanos(40)
        );
        assert_eq!(MonotonicTime::from_nanos(10) + Duration::INFINITE, MonotonicTime::INFINITE);
        assert_eq!(
            MonotonicTime::from_nanos(-10) + Duration::INFINITE_PAST,
            MonotonicTime::INFINITE_PAST
        );

        // Subtraction
        assert_eq!(
            MonotonicTime::from_nanos(10) - Duration::from_nanos(30),
            MonotonicTime::from_nanos(-20)
        );
        assert_eq!(
            MonotonicTime::from_nanos(-10) - Duration::INFINITE,
            MonotonicTime::INFINITE_PAST
        );
        assert_eq!(
            MonotonicTime::from_nanos(10) - Duration::INFINITE_PAST,
            MonotonicTime::INFINITE
        );

        // Assigning addition
        {
            let mut t = MonotonicTime::from_nanos(10);
            t += Duration::from_nanos(30);
            assert_eq!(t, MonotonicTime::from_nanos(40));
        }
        {
            let mut t = MonotonicTime::from_nanos(10);
            t += Duration::INFINITE;
            assert_eq!(t, MonotonicTime::INFINITE);
        }
        {
            let mut t = MonotonicTime::from_nanos(-10);
            t += Duration::INFINITE_PAST;
            assert_eq!(t, MonotonicTime::INFINITE_PAST);
        }

        // Assigning subtraction
        {
            let mut t = MonotonicTime::from_nanos(10);
            t -= Duration::from_nanos(30);
            assert_eq!(t, MonotonicTime::from_nanos(-20));
        }
        {
            let mut t = MonotonicTime::from_nanos(-10);
            t -= Duration::INFINITE;
            assert_eq!(t, MonotonicTime::INFINITE_PAST);
        }
        {
            let mut t = MonotonicTime::from_nanos(10);
            t -= Duration::INFINITE_PAST;
            assert_eq!(t, MonotonicTime::INFINITE);
        }
    }

    #[test]
    fn duration_saturation() {
        // Addition
        assert_eq!(Duration::from_nanos(10) + Duration::from_nanos(30), Duration::from_nanos(40));
        assert_eq!(Duration::from_nanos(10) + Duration::INFINITE, Duration::INFINITE);
        assert_eq!(Duration::from_nanos(-10) + Duration::INFINITE_PAST, Duration::INFINITE_PAST);

        // Subtraction
        assert_eq!(Duration::from_nanos(10) - Duration::from_nanos(30), Duration::from_nanos(-20));
        assert_eq!(Duration::from_nanos(-10) - Duration::INFINITE, Duration::INFINITE_PAST);
        assert_eq!(Duration::from_nanos(10) - Duration::INFINITE_PAST, Duration::INFINITE);

        // Multiplication
        assert_eq!(Duration::from_nanos(10) * 3, Duration::from_nanos(30));
        assert_eq!(Duration::from_nanos(10) * i64::MAX, Duration::INFINITE);
        assert_eq!(Duration::from_nanos(10) * i64::MIN, Duration::INFINITE_PAST);

        // Division
        assert_eq!(Duration::from_nanos(30) / 3, Duration::from_nanos(10));
        assert_eq!(Duration::INFINITE_PAST / -1, Duration::INFINITE);

        // Negation
        assert_eq!(-Duration::from_nanos(30), Duration::from_nanos(-30));
        assert_eq!(-Duration::INFINITE_PAST, Duration::INFINITE);

        // Assigning addition
        {
            let mut t = Duration::from_nanos(10);
            t += Duration::from_nanos(30);
            assert_eq!(t, Duration::from_nanos(40));
        }
        {
            let mut t = Duration::from_nanos(10);
            t += Duration::INFINITE;
            assert_eq!(t, Duration::INFINITE);
        }
        {
            let mut t = Duration::from_nanos(-10);
            t += Duration::INFINITE_PAST;
            assert_eq!(t, Duration::INFINITE_PAST);
        }

        // Assigning subtraction
        {
            let mut t = Duration::from_nanos(10);
            t -= Duration::from_nanos(30);
            assert_eq!(t, Duration::from_nanos(-20));
        }
        {
            let mut t = Duration::from_nanos(-10);
            t -= Duration::INFINITE;
            assert_eq!(t, Duration::INFINITE_PAST);
        }
        {
            let mut t = Duration::from_nanos(10);
            t -= Duration::INFINITE_PAST;
            assert_eq!(t, Duration::INFINITE);
        }
    }

    #[test]
    fn time_minus_time_saturates() {
        assert_eq!(MonotonicTime::INFINITE - MonotonicTime::INFINITE_PAST, Duration::INFINITE);
    }

    #[test]
    fn time_and_duration_defaults() {
        assert_eq!(MonotonicTime::default(), MonotonicTime::from_nanos(0));
        assert_eq!(Duration::default(), Duration::from_nanos(0));
    }
}
