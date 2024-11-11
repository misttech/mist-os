// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::diagnostics::{Diagnostics, Event};
use crate::enums::{
    ClockCorrectionStrategy, ClockUpdateReason, StartClockSource, Track, WriteRtcOutcome,
};
use crate::estimator::Estimator;
use crate::rtc::Rtc;
use crate::time_source_manager::{KernelBootTimeProvider, TimeSourceManager};
use crate::{Command, Config, UtcTransform};
use chrono::prelude::*;
use fuchsia_runtime::{UtcClock, UtcClockUpdate, UtcDuration, UtcInstant};
use futures::channel::mpsc;
use futures::{select, FutureExt, SinkExt, StreamExt};
use std::cell::Cell;
use std::cmp;
use std::fmt::{self, Debug};
use std::rc::Rc;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use zx::AsHandleRef;
use {fidl_fuchsia_time as fft, fuchsia_async as fasync};

/// One million for PPM calculations
const MILLION: i64 = 1_000_000;

/// The maximum clock frequency adjustment that Timekeeper will apply to slew away a UTC error,
/// expressed as a PPM deviation from nominal.
const MAX_RATE_CORRECTION_PPM: i64 = 200;

/// The clock frequency adjustment that Timekeeper will prefer when slewing away a UTC error,
/// expressed as a PPM deviation from nominal.
const NOMINAL_RATE_CORRECTION_PPM: i64 = 20;

/// The longest duration for which Timekeeper will apply a clock frequency adjustment in response to
/// a single time update.
const MAX_SLEW_DURATION: zx::BootDuration = zx::BootDuration::from_minutes(90);

/// The largest error that may be corrected through slewing at the maximum rate.
const MAX_RATE_MAX_ERROR: zx::BootDuration = zx::BootDuration::from_nanos(
    (MAX_SLEW_DURATION.into_nanos() * MAX_RATE_CORRECTION_PPM) / MILLION,
);

/// The largest error that may be corrected through slewing at the preferred rate.
const NOMINAL_RATE_MAX_ERROR: zx::BootDuration = zx::BootDuration::from_nanos(
    (MAX_SLEW_DURATION.into_nanos() * NOMINAL_RATE_CORRECTION_PPM) / MILLION,
);

/// The change in error bound that requires an update to the reported value. In some conditions
/// error bound may be updated even when it has changed by less than this value.
const ERROR_BOUND_UPDATE: u64 = 100_000_000; // 100ms

/// The interval at which the error bound will be refreshed while no other events are in progress.
const ERROR_REFRESH_INTERVAL: zx::BootDuration = zx::BootDuration::from_minutes(6);

/// Denotes an unknown clock error bound
const ZX_CLOCK_UNKNOWN_ERROR_BOUND: u64 = u64::MAX;

/// Describes how a correction will be made to a clock.
enum ClockCorrection {
    Step(Step),
    MaxDurationSlew(Slew),
    NominalRateSlew(Slew),
    /// Set the error bound to maximum known.
    MaxErrorBound,
}

impl ClockCorrection {
    /// Create a `ClockCorrection` to transition a clock from `initial_transform` to
    /// `final_transform` starting at a reference time of `start_time` and using standard policy for
    /// the rates and durations. Error bounds are calculated based on `final_transform`.
    fn for_transition(
        start_time: zx::BootInstant,
        initial_transform: &UtcTransform,
        final_transform: &UtcTransform,
    ) -> Self {
        let difference = final_transform.difference(&initial_transform, start_time);
        let difference_nanos = difference.into_nanos();
        let difference_abs = zx::BootDuration::from_nanos(difference_nanos.abs());
        let sign = if difference_nanos < 0 { -1 } else { 1 };

        if difference_abs < NOMINAL_RATE_MAX_ERROR {
            let rate = (NOMINAL_RATE_CORRECTION_PPM * sign) as i32;
            let duration = (difference_abs * MILLION) / NOMINAL_RATE_CORRECTION_PPM;
            ClockCorrection::NominalRateSlew(Slew::new(rate, duration, start_time, final_transform))
        } else if difference_abs < MAX_RATE_MAX_ERROR {
            // Round rate up to the next greater PPM such that duration will be slightly under
            // MAX_SLEW_DURATION rather that slightly over MAX_SLEW_DURATION.
            let rate = ((difference_nanos * MILLION) / MAX_SLEW_DURATION.into_nanos()) + sign;
            let duration = zx::BootDuration::from_nanos((difference.into_nanos() * MILLION) / rate);
            ClockCorrection::MaxDurationSlew(Slew::new(
                rate as i32,
                duration,
                start_time,
                final_transform,
            ))
        } else {
            ClockCorrection::Step(Step::new(difference, start_time, final_transform))
        }
    }

    /// Returns the total clock change made by this `ClockCorrection`.
    fn difference(&self) -> UtcDuration {
        match self {
            ClockCorrection::Step(step) => step.difference,
            ClockCorrection::NominalRateSlew(slew) | ClockCorrection::MaxDurationSlew(slew) => {
                UtcDuration::from_nanos(
                    (slew.duration.into_nanos() * (slew.slew_rate_adjust as i64)) / MILLION,
                )
            }
            // Setting max error bound does not result in a clock change, only the clock
            // error bound change.
            ClockCorrection::MaxErrorBound => UtcDuration::ZERO,
        }
    }

    /// Returns the strategy corresponding to this `ClockCorrection`.
    fn strategy(&self) -> ClockCorrectionStrategy {
        match self {
            ClockCorrection::Step(_) => ClockCorrectionStrategy::Step,
            ClockCorrection::NominalRateSlew(_) => ClockCorrectionStrategy::NominalRateSlew,
            ClockCorrection::MaxDurationSlew(_) => ClockCorrectionStrategy::MaxDurationSlew,
            ClockCorrection::MaxErrorBound => ClockCorrectionStrategy::MaxErrorBound,
        }
    }
}

/// Describes in detail how a clock will be stepped in order to correct time.
#[derive(PartialEq)]
struct Step {
    /// Change in clock value being made.
    difference: UtcDuration,
    /// Reference time at the step.
    reference: zx::BootInstant,
    /// UTC time after the step.
    utc: UtcInstant,
    /// Rate adjust in PPM after the step.
    rate_adjust_ppm: i32,
    /// Error bound after the step.
    error_bound: u64,
}

impl Step {
    /// Returns a step to move onto the supplied transform at the supplied reference time.
    fn new(
        difference: UtcDuration,
        start_time: zx::BootInstant,
        final_transform: &UtcTransform,
    ) -> Self {
        Step {
            difference,
            reference: start_time,
            utc: final_transform.synthetic(start_time),
            rate_adjust_ppm: final_transform.rate_adjust_ppm,
            error_bound: final_transform.error_bound(start_time),
        }
    }

    /// Returns a UtcClockUpdate describing the update to make to a clock to implement
    /// this `Step`.
    fn clock_update(&self) -> UtcClockUpdate {
        UtcClockUpdate::builder()
            .absolute_value(self.reference, self.utc)
            .rate_adjust(self.rate_adjust_ppm)
            .error_bounds(self.error_bound)
            .build()
    }
}

/// Describes in detail how a clock will be slewed in order to correct time.
#[derive(PartialEq)]
struct Slew {
    /// Clock rate adjustment to account for estimated oscillator error, in parts per million.
    base_rate_adjust: i32,
    /// Additional clock rate adjustment to execute the slew, in parts per million.
    slew_rate_adjust: i32,
    /// Duration for which the slew is to be maintained.
    duration: zx::BootDuration,
    /// Error bound at start of the slew.
    start_error_bound: u64,
    /// Error bound at completion of the slew.
    end_error_bound: u64,
}

impl Slew {
    /// Returns a fully defined slew to achieve the supplied parameters, calculating error bounds
    /// using the supplied transform and assuming the slew starts at the supplied reference time.
    fn new(
        slew_rate_adjust: i32,
        duration: zx::BootDuration,
        start_time: zx::BootInstant,
        final_transform: &UtcTransform,
    ) -> Self {
        let absolute_slew_correction_nanos =
            (duration.into_nanos() * (slew_rate_adjust as i64)).abs() / MILLION;
        Slew {
            base_rate_adjust: final_transform.rate_adjust_ppm,
            slew_rate_adjust,
            duration,
            // The initial error bound is the estimate error bound plus the entire correction.
            start_error_bound: final_transform.error_bound(start_time)
                + absolute_slew_correction_nanos as u64,
            end_error_bound: final_transform.error_bound(start_time + duration),
        }
    }

    /// Returns the overall rate adjustment applied while the slew is in progress, composed of the
    /// base rate due to oscillator error plus an additional rate to conduct the correction.
    fn total_rate_adjust(&self) -> i32 {
        self.base_rate_adjust + self.slew_rate_adjust
    }

    /// Returns a vector of (async::Time, UtcClockUpdate, ClockUpdateReason) tuples describing the
    /// updates to make to a clock during the slew. The first update is guaranteed to be requested
    /// immediately.
    fn clock_updates(&self) -> Vec<(fasync::BootInstant, UtcClockUpdate, ClockUpdateReason)> {
        // Note: fuchsia_async time can be mocked independently so can't assume its equivalent to
        // the supplied reference time.
        let start_time = fasync::BootInstant::now();
        let finish_time = start_time + zx::BootDuration::from_nanos(self.duration.into_nanos());

        // For large slews we expect the reduction in error bound while applying the correction to
        // exceed the growth in error bound due to oscillator error but there is no guarantee of
        // this. If error bound will increase through the slew, just use the worst case throughout.
        let begin_error_bound = cmp::max(self.start_error_bound, self.end_error_bound);

        // The final vector is composed of an initial update to start the slew...
        let mut updates = vec![(
            start_time,
            UtcClockUpdate::builder()
                .rate_adjust(self.total_rate_adjust())
                .error_bounds(begin_error_bound)
                .build(),
            ClockUpdateReason::BeginSlew,
        )];

        // ... intermediate updates to reduce the error bound if it reduces by more than the
        // threshold during the course of the slew ...
        if self.start_error_bound > self.end_error_bound + ERROR_BOUND_UPDATE {
            let bound_change = (self.start_error_bound - self.end_error_bound) as i64;
            let error_update_interval = zx::BootDuration::from_nanos(
                ((self.duration.into_nanos() as i128 * ERROR_BOUND_UPDATE as i128)
                    / bound_change as i128) as i64,
            );
            let mut i: i64 = 1;
            while start_time + error_update_interval * i < finish_time {
                updates.push((
                    start_time + error_update_interval * i,
                    UtcClockUpdate::builder()
                        .error_bounds(self.start_error_bound - ERROR_BOUND_UPDATE * i as u64)
                        .build(),
                    ClockUpdateReason::ReduceError,
                ));
                i += 1;
            }
        }

        // ... and a final update to return the rate to normal.
        updates.push((
            finish_time,
            UtcClockUpdate::builder()
                .rate_adjust(self.base_rate_adjust)
                .error_bounds(self.end_error_bound)
                .build(),
            ClockUpdateReason::EndSlew,
        ));
        updates
    }
}

impl Debug for Slew {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Slew")
            .field("rate_ppm", &self.slew_rate_adjust)
            .field("duration_ms", &self.duration.into_millis())
            .finish()
    }
}

/// Generates and applies all updates needed to maintain a userspace clock object (and optionally
/// also a real time clock) with accurate UTC time. New time samples are received from a
/// `TimeSourceManager` and a UTC estimate is produced based on these samples by an `Estimator`.
pub struct ClockManager<R: Rtc, D: Diagnostics> {
    /// The userspace clock to be maintained.
    clock: Arc<UtcClock>,
    /// The `TimeSourceManager` that supplies validated samples from a time source.
    time_source_manager: TimeSourceManager<D, KernelBootTimeProvider>,
    /// The `Estimator` that maintains an estimate of the UTC and frequency, populated after the
    /// first sample has been received.
    estimator: Option<Estimator<D>>,
    /// An optional real time clock that will be updated when new UTC estimates are produced.
    rtc: Option<R>,
    /// A diagnostics implementation for recording events of note.
    diagnostics: Arc<D>,
    /// The track of the estimate being managed.
    track: Track,
    /// A task used to complete any delayed clock updates, such as finishing a slew or increasing
    /// error bound in the absence of corrections.
    delayed_updates: Option<fasync::Task<()>>,
    /// Timekeeper config.
    config: Arc<Config>,
    // Test channel for signalization. Test only.
    test_signaler: Option<mpsc::Sender<()>>,
}

impl<R: Rtc, D: 'static + Diagnostics> ClockManager<R, D> {
    /// Construct a new `ClockManager` and start synchronizing the clock. The returned future
    /// will never complete.
    pub async fn execute(
        clock: Arc<UtcClock>,
        time_source_manager: TimeSourceManager<D, KernelBootTimeProvider>,
        rtc: Option<R>,
        diagnostics: Arc<D>,
        track: Track,
        config: Arc<Config>,
        async_commands: mpsc::Receiver<Command>,
        allow_update_rtc: Rc<Cell<bool>>,
    ) {
        ClockManager::new(clock, time_source_manager, rtc, diagnostics, track, config, None)
            .maintain_clock(async_commands, allow_update_rtc)
            .await
    }

    // TODO(jsankey): Once the network availability detection code is in the time sources (so we
    // no longer need to wait for network in between initializing the clock from rtc and
    // communicating with a time source) add an `execute_from_rtc` method that populates the clock
    // with the rtc value before beginning the maintain clock method.

    /// Construct a new `ClockManager`.
    fn new(
        clock: Arc<UtcClock>,
        time_source_manager: TimeSourceManager<D, KernelBootTimeProvider>,
        rtc: Option<R>,
        diagnostics: Arc<D>,
        track: Track,
        config: Arc<Config>,
        test_signaler: Option<mpsc::Sender<()>>,
    ) -> Self {
        debug!("creating clock manager");
        ClockManager {
            clock,
            time_source_manager,
            estimator: None,
            rtc,
            diagnostics,
            track,
            delayed_updates: None,
            config,
            test_signaler,
        }
    }

    /// Maintain the clock indefinitely. This future will never complete.
    ///
    /// Args:
    /// - `async_commands`: a channel with commands delivered to the maintain clock
    ///   loop.
    /// - `allow_update_rtc`: whether the maintain clock loop is allowed to update RTC.
    ///   This value can be changed by a concurrent but not parallel async fn.
    async fn maintain_clock(
        mut self,
        async_commands: mpsc::Receiver<Command>,
        allow_update_rtc: Rc<Cell<bool>>,
    ) {
        // TIMING NOTES: all delays in the management loops are tied to a monotonic
        // timeline, which means they can pause.
        debug!("maintain_clock: function entered");
        let pull_delay = self.config.get_back_off_time_between_pull_samples();
        let first_delay = self.config.get_first_sampling_delay();

        // Pause before first sampling is sometimes useful. But not if no delay
        // was ordered, so as not to change the scheduling order.
        if first_delay != zx::MonotonicDuration::ZERO {
            // This should be an uncommon setting, so log it.
            info!("delaying first time source sample by: {:?}", first_delay);
            _ = fasync::Timer::new(fasync::MonotonicInstant::after(first_delay)).await;
            debug!("delay done");
        }

        let details = self.clock.get_details().expect("failed to get UTC clock details");
        let mut clock_started = details.backstop != details.ticks_to_synthetic.synthetic_offset;
        std::mem::drop(details);
        let mut receiver = async_commands.fuse(); // Required by select! below.

        let serve_test_protocols = self.config.serve_test_protocols();
        let update_allowed = allow_update_rtc.get();
        let allow_timekeeper_to_update_rtc = !serve_test_protocols || update_allowed;
        if !allow_timekeeper_to_update_rtc {
            warn!(concat!(
                "RTC updates by Timekeeper are not allowed. ",
                "This is not a production setting. ",
                "Take note if you didn't expect this."
            ));
        }

        loop {
            debug!("manage_clock: asking for a time sample");

            // This may block for a *long* time if a sample is not available.
            let sample = self.time_source_manager.next_sample().await;
            debug!("manage_clock: `---- got a time sample: {:?}", sample);

            // Feed it to the estimator (or initialize the estimator).
            match &mut self.estimator {
                Some(estimator) => estimator.update(sample),
                None => {
                    self.estimator = Some(Estimator::new(
                        self.track,
                        sample,
                        Arc::clone(&self.diagnostics),
                        Arc::clone(&self.config),
                    ))
                }
            }
            // Note: Both branches of the match led to a populated estimator so safe to unwrap.
            let estimator: &mut Estimator<D> = &mut self.estimator.as_mut().unwrap();

            // Determine the intended reference->UTC transform and start or correct the clock.
            let estimate_transform = estimator.transform();
            if !clock_started {
                self.start_clock(&estimate_transform);
                clock_started = true;
            } else {
                self.apply_clock_correction(&estimate_transform).await;
            }

            if allow_timekeeper_to_update_rtc {
                // Update the RTC clock if we have one.
                self.update_rtc(&estimate_transform).await;
            }

            // Back off for a bit.
            let delay = if self.time_source_manager.is_suspendable_source() {
                debug!("backing off for pull source resampling.");
                pull_delay
            } else {
                zx::MonotonicDuration::from_millis(10)
            };
            debug!("clock_manager: waiting for command");
            select! {
                command = receiver.next() => {
                    debug!("received command: {:?}", &command);

                    match command {
                        Some(Command::PowerManagement) => {
                            self.record_correction(
                                ClockCorrection::MaxErrorBound, &Default::default(), zx::BootInstant::ZERO);
                        }
                        None => {
                            debug!("unexpected `None`");
                        }
                    }
                    // If a test signaler is present, acknowledge command receipt.
                    // The signaller will send a message when the channel is closed too.
                    if let Some(ref mut test_signaler) = self.test_signaler {
                        if let Err(ref e) = test_signaler.send(()).await {
                            warn!("could not acknowledge error on test_signaler: {:?}",  &e);
                        }
                    }
                },

                // If no command, then wait.
                _ = fasync::Timer::new(fasync::MonotonicInstant::after(delay)).fuse() => {
                    debug!("no commands received: tick");
                },
            }
        }
    }

    /// Starts the clock on the requested reference->utc transform, recording diagnostic events.
    fn start_clock(&mut self, estimate_transform: &UtcTransform) {
        let mono = zx::BootInstant::get();
        let clock_update = estimate_transform.jump_to(mono);
        self.update_clock(clock_update);

        self.diagnostics.record(Event::StartClock {
            track: self.track,
            source: StartClockSource::External(self.time_source_manager.role()),
        });

        let utc_chrono = Utc.timestamp_nanos(estimate_transform.synthetic(mono).into_nanos());
        info!("started {:?} clock from external source at {}", self.track, utc_chrono);
        self.set_delayed_update_task(vec![], estimate_transform);
    }

    /// Applies a correction to the clock to reach the requested reference->utc transform, selecting
    /// and applying the most appropriate strategy and recording diagnostic events.
    async fn apply_clock_correction(&mut self, estimate_transform: &UtcTransform) {
        // Any pending clock updates will be superseded by the handling of this one.
        self.delayed_updates = None;

        let current_transform = UtcTransform::from(self.clock.as_ref());
        let mono = zx::BootInstant::get();

        let correction =
            ClockCorrection::for_transition(mono, &current_transform, estimate_transform);
        self.record_correction(correction, estimate_transform, mono);
    }

    /// Records this particular clock correction.
    fn record_correction(
        &mut self,
        correction: ClockCorrection,
        estimate_transform: &UtcTransform,
        mono: zx::BootInstant,
    ) {
        self.record_clock_correction(correction.difference(), correction.strategy());
        match correction {
            ClockCorrection::MaxDurationSlew(slew) | ClockCorrection::NominalRateSlew(slew) => {
                let mut updates = slew.clock_updates();
                // The first update is guaranteed to be immediate.
                let (_, clock_update, reason) = updates.remove(0);
                self.update_clock(clock_update);
                self.record_clock_update(reason);

                info!(
                    "started {:?} {:?} with {} scheduled updates",
                    self.track,
                    slew,
                    updates.len()
                );

                // Create a task to asynchronously apply all the remaining updates and then increase
                // error bound over time.
                self.set_delayed_update_task(updates, estimate_transform);
            }
            ClockCorrection::Step(step) => {
                self.update_clock(step.clock_update());
                self.record_clock_update(ClockUpdateReason::TimeStep);

                let utc_chrono =
                    Utc.timestamp_nanos(estimate_transform.synthetic(mono).into_nanos());
                info!("stepped {:?} clock to {}", self.track, utc_chrono);

                // Create a task to asynchronously increase error bound over time.
                self.set_delayed_update_task(vec![], estimate_transform);
            }
            ClockCorrection::MaxErrorBound => {
                let update =
                    UtcClockUpdate::builder().error_bounds(ZX_CLOCK_UNKNOWN_ERROR_BOUND).build();
                self.update_clock(update);
                info!("set clock error bound to maximum for track: {:?}", self.track);
            }
        };
    }

    /// Updates the real time clock to the supplied transform if an RTC is configured.
    async fn update_rtc(&mut self, estimate_transform: &UtcTransform) {
        // Note RTC only applies to primary so we don't include the track in our log messages.
        if let Some(ref rtc) = self.rtc {
            let estimate_utc = estimate_transform.synthetic(zx::BootInstant::get());
            let utc_chrono = Utc.timestamp_nanos(estimate_utc.into_nanos());
            match rtc.set(estimate_utc).await {
                Err(err) => {
                    error!("failed to update RTC to {}: {}", utc_chrono, err);
                    self.diagnostics.record(Event::WriteRtc { outcome: WriteRtcOutcome::Failed });
                }
                Ok(()) => {
                    info!("updated RTC to {}", utc_chrono);
                    self.diagnostics
                        .record(Event::WriteRtc { outcome: WriteRtcOutcome::Succeeded });
                }
            };
        }
    }

    /// Sets a task that asynchronously applies the supplied scheduled clock updates and then
    /// periodically applies an increase in the error bound indefinitely based on supplied estimate
    /// transform.
    fn set_delayed_update_task(
        &mut self,
        scheduled_updates: Vec<(fasync::BootInstant, UtcClockUpdate, ClockUpdateReason)>,
        estimate_transform: &UtcTransform,
    ) {
        let clock = Arc::clone(&self.clock);
        let diagnostics = Arc::clone(&self.diagnostics);
        let track = self.track;
        let transform = estimate_transform.clone();

        let async_now = fasync::BootInstant::now();
        // The first periodic step in error bound occurs a fixed duration after the last
        // scheduled update or after current time if no scheduled updates were supplied.
        let mut step_async_time =
            scheduled_updates.last().map(|tup| tup.0).unwrap_or(async_now) + ERROR_REFRESH_INTERVAL;
        // Updates are supplied in fuchsia_async time for ease of scheduling and unit testing, but
        // we need to calculate a corresponding reference time to read the absolute error bound.
        let mut step_mono_time = zx::BootInstant::get() + (step_async_time - async_now);

        self.delayed_updates = Some(fasync::Task::spawn(async move {
            for (update_time, update, reason) in scheduled_updates.into_iter() {
                fasync::Timer::new(update_time).await;
                info!("executing scheduled {:?} clock update: {:?}, {:?}", track, reason, update);
                update_clock(&clock, &track, update);
                diagnostics.record(Event::UpdateClock { track, reason });
            }

            loop {
                fasync::Timer::new(step_async_time).await;
                let step_error_bound = transform.error_bound(step_mono_time);
                info!("increasing {:?} error bound to {:?}ns", track, step_error_bound);
                update_clock(
                    &clock,
                    &track,
                    UtcClockUpdate::builder().error_bounds(step_error_bound),
                );
                diagnostics
                    .record(Event::UpdateClock { track, reason: ClockUpdateReason::IncreaseError });
                step_async_time += ERROR_REFRESH_INTERVAL;
                step_mono_time += ERROR_REFRESH_INTERVAL;
            }
        }));
    }

    /// Applies an update to the clock.
    fn update_clock(&mut self, update: impl Into<UtcClockUpdate>) {
        update_clock(&self.clock, &self.track, update);
    }

    /// Records the reason for a clock update with diagnostics.
    fn record_clock_update(&self, reason: ClockUpdateReason) {
        self.diagnostics.record(Event::UpdateClock { track: self.track, reason });
    }

    /// Records a correction to the clock with diagnostics.
    fn record_clock_correction(&self, correction: UtcDuration, strategy: ClockCorrectionStrategy) {
        self.diagnostics.record(Event::ClockCorrection { track: self.track, correction, strategy });
    }
}

/// Applies an update to the supplied clock, panicking with a comprehensible error on failure.
fn update_clock(clock: &Arc<UtcClock>, track: &Track, update: impl Into<UtcClockUpdate>) {
    if let Err(status) = clock.update(update) {
        // Clock update errors should only be caused by an invalid clock (or potentially a
        // serious bug in the generation of a time update). There isn't anything Timekeeper
        // could do to gracefully handle them.
        panic!("Failed to apply update to {:?} clock: {}", track, status);
    }
    // Signal any waiters that the UTC clock has been synchronized with an external
    // time source.
    if let Err(status) = clock.signal_handle(
        zx::Signals::NONE,
        zx::Signals::from_bits(
            fft::SIGNAL_UTC_CLOCK_SYNCHRONIZED | fft::SIGNAL_UTC_CLOCK_LOGGING_QUALITY,
        )
        .unwrap(),
    ) {
        panic!("Failed to signal clock synchronization to {:?} clock: {}", track, status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{FakeDiagnostics, ANY_DURATION};
    use crate::enums::{FrequencyDiscardReason, Role};
    use crate::rtc::FakeRtc;
    use crate::time_source::{Event as TimeSourceEvent, FakePushTimeSource, Sample};
    use crate::{make_test_config, make_test_config_with_delay};
    use fidl_fuchsia_time_external::{self as ftexternal, Status};
    use fuchsia_async as fasync;
    use lazy_static::lazy_static;
    use std::pin::pin;
    use test_util::{assert_geq, assert_gt, assert_leq, assert_lt, assert_near};

    const NANOS_PER_SECOND: i64 = 1_000_000_000;
    const TEST_ROLE: Role = Role::Primary;
    const SAMPLE_SPACING: zx::BootDuration = zx::BootDuration::from_millis(100);
    const OFFSET: zx::BootDuration = zx::BootDuration::from_seconds(1111_000);
    const OFFSET_2: zx::BootDuration = zx::BootDuration::from_seconds(2222_000);
    const BASE_RATE: i32 = -9;
    const BASE_RATE_2: i32 = 2;
    const STD_DEV: zx::BootDuration = zx::BootDuration::from_millis(88);
    const BACKSTOP_TIME: UtcInstant = UtcInstant::from_nanos(222222 * NANOS_PER_SECOND);
    const ERROR_GROWTH_PPM: u32 = 30;

    lazy_static! {
        static ref TEST_TRACK: Track = Track::from(TEST_ROLE);
        static ref CLOCK_OPTS: zx::ClockOpts = zx::ClockOpts::empty();
        static ref START_CLOCK_SOURCE: StartClockSource = StartClockSource::External(TEST_ROLE);
    }

    /// Creates and starts a new clock with default options.
    fn create_clock() -> Arc<UtcClock> {
        let clock = UtcClock::create(*CLOCK_OPTS, Some(BACKSTOP_TIME)).unwrap();
        clock.update(UtcClockUpdate::builder().approximate_value(BACKSTOP_TIME)).unwrap();
        Arc::new(clock)
    }

    /// Creates a new `ClockManager` from a time source manager that outputs the supplied samples.
    fn create_clock_manager(
        clock: Arc<UtcClock>,
        samples: Vec<Sample>,
        final_time_source_status: Option<ftexternal::Status>,
        rtc: Option<FakeRtc>,
        diagnostics: Arc<FakeDiagnostics>,
        config: Arc<Config>,
        test_signaler: Option<mpsc::Sender<()>>,
    ) -> ClockManager<FakeRtc, FakeDiagnostics> {
        let mut events: Vec<TimeSourceEvent> =
            samples.into_iter().map(|sample| TimeSourceEvent::from(sample)).collect();
        events.insert(0, TimeSourceEvent::StatusChange { status: ftexternal::Status::Ok });
        if let Some(status) = final_time_source_status {
            events.push(TimeSourceEvent::StatusChange { status });
        }
        let time_source = FakePushTimeSource::events(events).into();
        let time_source_manager = TimeSourceManager::new_with_delays_disabled(
            BACKSTOP_TIME,
            TEST_ROLE,
            time_source,
            Arc::clone(&diagnostics),
        );
        ClockManager::new(
            clock,
            time_source_manager,
            rtc,
            diagnostics,
            *TEST_TRACK,
            config,
            test_signaler,
        )
    }

    /// Creates a new transform.
    fn create_transform(
        reference: zx::BootInstant,
        synthetic: UtcInstant,
        rate_adjust_ppm: i32,
        std_dev: zx::BootDuration,
    ) -> UtcTransform {
        UtcTransform {
            reference_offset: reference,
            synthetic_offset: synthetic,
            rate_adjust_ppm,
            error_bound_at_offset: 2 * std_dev.into_nanos() as u64,
            error_bound_growth_ppm: ERROR_GROWTH_PPM,
        }
    }

    #[fuchsia::test]
    fn clock_correction_for_transition_nominal_rate_slew() {
        let mono = zx::BootInstant::get();
        // Note the initial transform has a reference point before reference and has been
        // running since then with a small rate adjustment.
        let initial_transform = create_transform(
            mono - zx::BootDuration::from_minutes(1),
            UtcInstant::from_nanos(
                (mono - zx::BootDuration::from_minutes(1) + OFFSET).into_nanos(),
            ),
            BASE_RATE,
            zx::BootDuration::from_nanos(0),
        );
        let final_transform = create_transform(
            mono,
            UtcInstant::from_nanos(
                (mono + OFFSET + zx::BootDuration::from_millis(50)).into_nanos(),
            ),
            BASE_RATE_2,
            STD_DEV,
        );

        let correction =
            ClockCorrection::for_transition(mono, &initial_transform, &final_transform);
        let expected_difference = zx::BootDuration::from_nanos(
            zx::BootDuration::from_minutes(1).into_nanos() * -BASE_RATE as i64 / MILLION,
        ) + zx::BootDuration::from_millis(50);
        assert_eq!(
            correction.difference(),
            UtcDuration::from_nanos(expected_difference.into_nanos())
        );

        // The values chosen for rates and offset differences mean this is a nominal rate slew.
        let expected_duration = (expected_difference * MILLION) / NOMINAL_RATE_CORRECTION_PPM;
        let expected_start_error_bound =
            final_transform.error_bound(mono) + expected_difference.into_nanos() as u64;
        let expected_end_error_bound = final_transform.error_bound(mono + expected_duration);

        assert_eq!(correction.strategy(), ClockCorrectionStrategy::NominalRateSlew);
        match correction {
            ClockCorrection::NominalRateSlew(slew) => {
                assert_eq!(slew.base_rate_adjust, BASE_RATE_2);
                assert_eq!(slew.slew_rate_adjust, NOMINAL_RATE_CORRECTION_PPM as i32);
                assert_eq!(slew.total_rate_adjust(), slew.slew_rate_adjust + slew.base_rate_adjust);
                assert_eq!(slew.duration, expected_duration);
                assert_eq!(slew.start_error_bound, expected_start_error_bound);
                assert_eq!(slew.end_error_bound, expected_end_error_bound);
            }
            _ => panic!("incorrect clock correction type returned"),
        }
    }

    #[fuchsia::test]
    fn clock_correction_for_transition_max_duration_slew() {
        let mono = zx::BootInstant::get();
        let initial_transform = create_transform(
            mono,
            UtcInstant::from_nanos((mono + OFFSET).into_nanos()),
            BASE_RATE,
            STD_DEV,
        );
        let final_transform = create_transform(
            mono,
            UtcInstant::from_nanos(
                (mono + OFFSET - zx::BootDuration::from_millis(500)).into_nanos(),
            ),
            BASE_RATE,
            STD_DEV,
        );

        let correction =
            ClockCorrection::for_transition(mono, &initial_transform, &final_transform);
        let expected_difference = zx::BootDuration::from_millis(-500);
        // Note there is a slight loss of precision in converting from a difference to a rate for
        // a duration.
        assert_near!(correction.difference().into_nanos(), expected_difference.into_nanos(), 10);

        // The value chosen for offset difference means this is a max duration slew.
        let expected_duration = zx::BootDuration::from_nanos(5376344086021);
        let expected_rate = -93;
        let expected_start_error_bound =
            final_transform.error_bound(mono) + correction.difference().into_nanos().abs() as u64;
        let expected_end_error_bound = final_transform.error_bound(mono + expected_duration);

        assert_eq!(correction.strategy(), ClockCorrectionStrategy::MaxDurationSlew);
        match correction {
            ClockCorrection::MaxDurationSlew(slew) => {
                assert_eq!(slew.base_rate_adjust, BASE_RATE);
                assert_eq!(slew.slew_rate_adjust, expected_rate);
                assert_eq!(slew.total_rate_adjust(), slew.slew_rate_adjust + slew.base_rate_adjust);
                assert_eq!(slew.duration, expected_duration);
                assert_eq!(slew.start_error_bound, expected_start_error_bound);
                assert_eq!(slew.end_error_bound, expected_end_error_bound);
            }
            _ => panic!("incorrect clock correction type returned"),
        }
    }

    #[fuchsia::test]
    fn clock_correction_for_transition_step() {
        let mono = zx::BootInstant::get();
        let initial_transform = create_transform(
            mono - zx::BootDuration::from_minutes(1),
            UtcInstant::from_nanos(
                (mono - zx::BootDuration::from_minutes(1) + OFFSET).into_nanos(),
            ),
            0,
            zx::BootDuration::from_nanos(0),
        );
        let final_transform = create_transform(
            mono,
            UtcInstant::from_nanos((mono + OFFSET + zx::BootDuration::from_hours(1)).into_nanos()),
            BASE_RATE_2,
            STD_DEV,
        );

        let correction =
            ClockCorrection::for_transition(mono, &initial_transform, &final_transform);
        let expected_difference = UtcDuration::from_hours(1);
        assert_eq!(correction.difference(), expected_difference);
        assert_eq!(correction.strategy(), ClockCorrectionStrategy::Step);
        match correction {
            ClockCorrection::Step(step) => {
                assert_eq!(step.clock_update(), final_transform.jump_to(mono));
            }
            _ => panic!("incorrect clock correction type returned"),
        };
    }

    #[fuchsia::test]
    fn slew_clock_updates_fn() {
        let executor = fasync::TestExecutor::new_with_fake_time();
        executor.set_fake_time(fasync::MonotonicInstant::from_nanos(0));

        // Simple constructor lambdas to improve readability of the test logic.
        let full_update = |rate: i32, error_bound: u64| -> UtcClockUpdate {
            UtcClockUpdate::builder().rate_adjust(rate).error_bounds(error_bound).build()
        };
        let error_update = |error_bound: u64| -> UtcClockUpdate {
            UtcClockUpdate::builder().error_bounds(error_bound).build()
        };
        let time_seconds = |seconds: i64| -> fasync::BootInstant {
            fasync::BootInstant::from_nanos(seconds * NANOS_PER_SECOND)
        };

        // A short slew should contain no error bound updates.
        let slew = Slew {
            base_rate_adjust: 5,
            slew_rate_adjust: -20,
            duration: zx::BootDuration::from_seconds(10),
            start_error_bound: 9_000_000,
            end_error_bound: 1_000_000,
        };
        assert_eq!(
            slew.clock_updates(),
            vec![
                (
                    time_seconds(0),
                    full_update(slew.total_rate_adjust(), slew.start_error_bound),
                    ClockUpdateReason::BeginSlew
                ),
                (
                    time_seconds(slew.duration.into_seconds()),
                    full_update(slew.base_rate_adjust, slew.end_error_bound),
                    ClockUpdateReason::EndSlew
                ),
            ]
        );

        // A larger slew should contain as many error bound reductions as needed.
        let slew = Slew {
            base_rate_adjust: 3,
            slew_rate_adjust: 80,
            duration: zx::BootDuration::from_hours(10),
            start_error_bound: 800_000_000,
            end_error_bound: 550_000_000,
        };
        let update_interval_nanos =
            ((slew.duration.into_nanos() as i128 * ERROR_BOUND_UPDATE as i128)
                / (slew.start_error_bound - slew.end_error_bound) as i128) as i64;
        assert_eq!(
            slew.clock_updates(),
            vec![
                (
                    time_seconds(0),
                    full_update(slew.total_rate_adjust(), slew.start_error_bound),
                    ClockUpdateReason::BeginSlew
                ),
                (
                    fasync::BootInstant::from_nanos(update_interval_nanos),
                    error_update(slew.start_error_bound - ERROR_BOUND_UPDATE),
                    ClockUpdateReason::ReduceError,
                ),
                (
                    fasync::BootInstant::from_nanos(2 * update_interval_nanos),
                    error_update(slew.start_error_bound - 2 * ERROR_BOUND_UPDATE),
                    ClockUpdateReason::ReduceError,
                ),
                (
                    time_seconds(slew.duration.into_seconds()),
                    full_update(slew.base_rate_adjust, slew.end_error_bound),
                    ClockUpdateReason::EndSlew
                ),
            ]
        );

        // When the error reduction from applying the correction is smaller than the growth from the
        // oscillator uncertainty the error bound should be fixed at the final value with no
        // intermediate updates.
        let slew = Slew {
            base_rate_adjust: 0,
            slew_rate_adjust: -2,
            duration: zx::BootDuration::from_hours(10),
            start_error_bound: 200_000_000,
            end_error_bound: 800_000_000,
        };
        assert_eq!(
            slew.clock_updates(),
            vec![
                (
                    time_seconds(0),
                    full_update(slew.total_rate_adjust(), slew.end_error_bound),
                    ClockUpdateReason::BeginSlew
                ),
                (
                    time_seconds(slew.duration.into_seconds()),
                    full_update(slew.base_rate_adjust, slew.end_error_bound),
                    ClockUpdateReason::EndSlew
                ),
            ]
        );
    }

    #[fuchsia::test]
    fn single_update_with_rtc() {
        let mut executor = fasync::TestExecutor::new();

        let clock = create_clock();
        let rtc = FakeRtc::valid(BACKSTOP_TIME);
        let diagnostics = Arc::new(FakeDiagnostics::new());
        let config = make_test_config();

        // Create a clock manager.
        let reference = zx::BootInstant::get();
        let clock_manager = create_clock_manager(
            Arc::clone(&clock),
            vec![Sample::new(
                UtcInstant::from_nanos((reference + OFFSET).into_nanos()),
                reference,
                STD_DEV,
            )],
            None,
            Some(rtc.clone()),
            Arc::clone(&diagnostics),
            config,
            None,
        );

        // Maintain the clock until no more work remains.
        let reference_before = zx::BootInstant::get();
        let (_, r) = mpsc::channel(1);
        let allow_rtc = Rc::new(Cell::new(true));
        let mut fut = pin!(clock_manager.maintain_clock(r, allow_rtc));
        let _ = executor.run_until_stalled(&mut fut);
        let updated_utc = clock.read().unwrap();
        let reference_after = zx::BootInstant::get();

        // Check that the clocks have been updated. The UTC should be bounded by the offset we
        // supplied added to the reference window in which the calculation took place.
        assert_geq!(updated_utc.into_nanos(), (reference_before + OFFSET).into_nanos());
        assert_leq!(updated_utc.into_nanos(), (reference_after + OFFSET).into_nanos());
        assert_geq!(rtc.last_set().unwrap().into_nanos(), (reference_before + OFFSET).into_nanos());
        assert_leq!(rtc.last_set().unwrap().into_nanos(), (reference_after + OFFSET).into_nanos());

        // Check that the correct diagnostic events were logged.
        diagnostics.assert_events(&[
            Event::TimeSourceStatus { role: TEST_ROLE, status: Status::Ok },
            Event::KalmanFilterUpdated {
                track: *TEST_TRACK,
                reference: reference,
                utc: UtcInstant::from_nanos((reference + OFFSET).into_nanos()),
                sqrt_covariance: STD_DEV,
            },
            Event::StartClock { track: *TEST_TRACK, source: *START_CLOCK_SOURCE },
            Event::WriteRtc { outcome: WriteRtcOutcome::Succeeded },
        ]);
    }

    #[fuchsia::test]
    fn fail_when_no_initial_delay() {
        let mut executor = fasync::TestExecutor::new_with_fake_time();

        let clock = create_clock();
        let rtc = FakeRtc::valid(BACKSTOP_TIME);
        let diagnostics = Arc::new(FakeDiagnostics::new());
        let config = make_test_config_with_delay(1);

        // Create a clock manager.
        let reference = zx::BootInstant::get();
        let clock_manager = create_clock_manager(
            Arc::clone(&clock),
            vec![Sample::new(
                UtcInstant::from_nanos((reference + OFFSET).into_nanos()),
                reference,
                STD_DEV,
            )],
            None,
            Some(rtc.clone()),
            Arc::clone(&diagnostics),
            config,
            None,
        );

        let (_, r) = mpsc::channel(1);
        let start_time = executor.now();
        let b = Rc::new(Cell::new(true));
        let mut fut = pin!(clock_manager.maintain_clock(r, b));

        let _ = executor.run_until_stalled(&mut fut);

        // Half a second in, nothing to wake.
        executor.set_fake_time(start_time + fasync::MonotonicDuration::from_millis(550));
        let _ = executor.run_until_stalled(&mut fut);
        assert_eq!(false, executor.wake_expired_timers());

        // One second in, there is something to wake.
        executor.set_fake_time(start_time + fasync::MonotonicDuration::from_millis(1050));
        assert_eq!(true, executor.wake_expired_timers());
    }

    #[fuchsia::test]
    fn verify_asynchronous_signal_honored() {
        let mut executor = fasync::LocalExecutor::new();
        let (test_sender, mut test_received) = mpsc::channel(1);
        let (mut s, r) = mpsc::channel(1);
        let clock = create_clock();
        clock.update(UtcClockUpdate::builder().error_bounds(0).build()).unwrap();

        let clock_clone = clock.clone();

        // At the start, the clock is set to no error.
        let details = clock_clone.get_details().unwrap();
        assert_eq!(0, details.error_bounds);
        let b = Rc::new(Cell::new(true));

        let _t = fasync::Task::local(async move {
            // Set zero bound.
            let rtc = FakeRtc::valid(BACKSTOP_TIME);
            let diagnostics = Arc::new(FakeDiagnostics::new());
            let config = make_test_config();

            let reference = zx::BootInstant::get();
            let clock_manager = create_clock_manager(
                Arc::clone(&clock),
                vec![Sample::new(
                    UtcInstant::from_nanos((reference + OFFSET).into_nanos()),
                    reference,
                    STD_DEV,
                )],
                None,
                Some(rtc.clone()),
                Arc::clone(&diagnostics),
                config,
                Some(test_sender),
            );
            clock_manager.maintain_clock(r, b).await;
        });

        // Signal that clock update is needed.
        let _s = fasync::Task::local(async move {
            s.send(Command::PowerManagement).await.unwrap();
        });

        // Wait until the signal propagates.
        let ret = executor.run_singlethreaded(async move { test_received.next().await });
        assert_eq!(Some(()), ret);

        // Verify that clock now has a large error bound.
        let details = clock_clone.get_details().unwrap();
        assert_eq!(ZX_CLOCK_UNKNOWN_ERROR_BOUND, details.error_bounds);
    }

    #[fuchsia::test]
    fn verify_asynchronous_test_signal_honored() {
        let mut executor = fasync::LocalExecutor::new();
        let (test_sender, mut test_received) = mpsc::channel(1);
        let (mut s, r) = mpsc::channel(1);
        let clock = create_clock();
        clock.update(UtcClockUpdate::builder().error_bounds(0).build()).unwrap();

        let _clock_manager_task = fasync::Task::local(async move {
            // Set zero bound.
            let rtc = FakeRtc::valid(BACKSTOP_TIME);
            let diagnostics = Arc::new(FakeDiagnostics::new());
            let config = crate::tests::make_test_config_with_test_protocols();
            let b = Rc::new(Cell::new(true));

            let reference = zx::BootInstant::get();
            let clock_manager = create_clock_manager(
                Arc::clone(&clock),
                vec![Sample::new(
                    UtcInstant::from_nanos((reference + OFFSET).into_nanos()),
                    reference,
                    STD_DEV,
                )],
                None,
                Some(rtc.clone()),
                Arc::clone(&diagnostics),
                config,
                Some(test_sender),
            );
            clock_manager.maintain_clock(r, b).await;
        });

        // Signal that clock update is needed.
        let _signal_rtc_command = fasync::Task::local(async move {
            s.send(Command::PowerManagement).await.unwrap();
        });

        // Wait until the signal propagates.
        let ret = executor.run_singlethreaded(async move { test_received.next().await });
        assert_eq!(Some(()), ret);
    }

    #[fuchsia::test]
    fn single_update_without_rtc() {
        let mut executor = fasync::TestExecutor::new();

        let clock = create_clock();
        let diagnostics = Arc::new(FakeDiagnostics::new());
        let reference = zx::BootInstant::get();
        let config = make_test_config();
        let clock_manager = create_clock_manager(
            Arc::clone(&clock),
            vec![Sample::new(
                UtcInstant::from_nanos((reference + OFFSET).into_nanos()),
                reference,
                STD_DEV,
            )],
            None,
            None,
            Arc::clone(&diagnostics),
            config,
            None,
        );

        // Maintain the clock until no more work remains
        let reference_before = zx::BootInstant::get();
        let (_, r) = mpsc::channel(1);
        let b = Rc::new(Cell::new(true));
        let mut fut = pin!(clock_manager.maintain_clock(r, b));
        let _ = executor.run_until_stalled(&mut fut);
        let updated_utc = clock.read().unwrap();
        let reference_after = zx::BootInstant::get();

        // Check that the clock has been updated. The UTC should be bounded by the offset we
        // supplied added to the reference window in which the calculation took place.
        assert_geq!(updated_utc.into_nanos(), (reference_before + OFFSET).into_nanos());
        assert_leq!(updated_utc.into_nanos(), (reference_after + OFFSET).into_nanos());

        // If we keep waiting the error bound should increase in the absence of updates.
        let details1 = clock.get_details().unwrap();
        assert!(executor.wake_next_boot_timer().is_some());
        let _ = executor.run_until_stalled(&mut fut);
        let details2 = clock.get_details().unwrap();
        assert_eq!(details2.reference_to_synthetic, details1.reference_to_synthetic);
        assert_gt!(details2.error_bounds, details1.error_bounds);
        assert!(executor.wake_next_boot_timer().is_some());
        let _ = executor.run_until_stalled(&mut fut);
        let details3 = clock.get_details().unwrap();
        assert_eq!(details3.reference_to_synthetic, details1.reference_to_synthetic);
        assert_gt!(details3.error_bounds, details2.error_bounds);

        // Check that the correct diagnostic events were logged.
        diagnostics.assert_events(&[
            Event::TimeSourceStatus { role: TEST_ROLE, status: Status::Ok },
            Event::KalmanFilterUpdated {
                track: *TEST_TRACK,
                reference: reference,
                utc: UtcInstant::from_nanos((reference + OFFSET).into_nanos()),
                sqrt_covariance: STD_DEV,
            },
            Event::StartClock { track: *TEST_TRACK, source: *START_CLOCK_SOURCE },
            Event::UpdateClock { track: *TEST_TRACK, reason: ClockUpdateReason::IncreaseError },
            Event::UpdateClock { track: *TEST_TRACK, reason: ClockUpdateReason::IncreaseError },
        ]);
    }

    #[fuchsia::test]
    fn subsequent_updates_accepted() {
        let mut executor = fasync::TestExecutor::new();

        let clock = create_clock();
        let diagnostics = Arc::new(FakeDiagnostics::new());
        let reference = zx::BootInstant::get();
        let config = make_test_config();
        let clock_manager = create_clock_manager(
            Arc::clone(&clock),
            vec![
                Sample::new(
                    UtcInstant::from_nanos((reference - SAMPLE_SPACING + OFFSET).into_nanos()),
                    reference - SAMPLE_SPACING,
                    STD_DEV,
                ),
                Sample::new(
                    UtcInstant::from_nanos((reference + OFFSET_2).into_nanos()),
                    reference,
                    STD_DEV,
                ),
            ],
            None,
            None,
            Arc::clone(&diagnostics),
            config,
            None,
        );

        // Maintain the clock until no more work remains
        let reference_before = zx::BootInstant::get();
        let (_, r) = mpsc::channel(1);
        let b = Rc::new(Cell::new(true));
        let mut fut = pin!(clock_manager.maintain_clock(r, b));
        let _ = executor.run_until_stalled(&mut fut);
        let updated_utc = clock.read().unwrap();
        let reference_after = zx::BootInstant::get();

        // Since we used the same covariance for the first two samples the offset in the Kalman
        // filter is roughly midway between the sample offsets, but slight closer to the second
        // because oscillator uncertainty.
        let expected_offset = zx::BootDuration::from_nanos(1666500000080699);

        // Check that the clock has been updated. The UTC should be bounded by the expected offset
        // added to the reference window in which the calculation took place.
        assert_geq!(updated_utc.into_nanos(), (reference_before + expected_offset).into_nanos());
        assert_leq!(updated_utc.into_nanos(), (reference_after + expected_offset).into_nanos());

        // Check that the correct diagnostic events were logged.
        diagnostics.assert_events(&[
            Event::TimeSourceStatus { role: TEST_ROLE, status: Status::Ok },
            Event::KalmanFilterUpdated {
                track: *TEST_TRACK,
                reference: reference - SAMPLE_SPACING,
                utc: UtcInstant::from_nanos((reference - SAMPLE_SPACING + OFFSET).into_nanos()),
                sqrt_covariance: STD_DEV,
            },
            Event::StartClock { track: *TEST_TRACK, source: *START_CLOCK_SOURCE },
            Event::KalmanFilterUpdated {
                track: *TEST_TRACK,
                reference: reference,
                utc: UtcInstant::from_nanos((reference + expected_offset).into_nanos()),
                sqrt_covariance: zx::BootDuration::from_nanos(62225396),
            },
            Event::FrequencyWindowDiscarded {
                track: *TEST_TRACK,
                reason: FrequencyDiscardReason::TimeStep,
            },
            Event::ClockCorrection {
                track: *TEST_TRACK,
                correction: UtcDuration::from_nanos(ANY_DURATION.into_nanos()),
                strategy: ClockCorrectionStrategy::Step,
            },
            Event::UpdateClock { track: *TEST_TRACK, reason: ClockUpdateReason::TimeStep },
        ]);
    }

    #[fuchsia::test]
    fn correction_by_slew() {
        let mut executor = fasync::TestExecutor::new();

        // Calculate a small change in offset that will be corrected by slewing and is large enough
        // to require an error bound reduction. Note the tests doesn't have to actually wait this
        // long since we can manually trigger async timers.
        let delta_offset = zx::BootDuration::from_millis(600);
        let filtered_delta_offset = delta_offset / 2;

        let clock = create_clock();
        let diagnostics = Arc::new(FakeDiagnostics::new());
        let reference = zx::BootInstant::get();
        let config = make_test_config();
        let clock_manager = create_clock_manager(
            Arc::clone(&clock),
            vec![
                Sample::new(
                    UtcInstant::from_nanos((reference - SAMPLE_SPACING + OFFSET).into_nanos()),
                    reference - SAMPLE_SPACING,
                    STD_DEV,
                ),
                Sample::new(
                    UtcInstant::from_nanos((reference + OFFSET + delta_offset).into_nanos()),
                    reference,
                    STD_DEV,
                ),
            ],
            // Leave the time source in network unavailable after its sent the samples so it
            // doesn't get killed for being unresponsive while the slew is applied.
            Some(ftexternal::Status::Network),
            None,
            Arc::clone(&diagnostics),
            config,
            None,
        );

        // Maintain the clock until no more work remains, which should correspond to having started
        // a clock skew but blocking on the timer to end it.
        let reference_before = zx::BootInstant::get();
        let (_, r) = mpsc::channel(1);
        let b = Rc::new(Cell::new(true));
        let mut fut = pin!(clock_manager.maintain_clock(r, b));
        let _ = executor.run_until_stalled(&mut fut);
        let updated_utc = clock.read().unwrap();
        let details = clock.get_details().unwrap();
        let reference_after = zx::BootInstant::get();

        // The clock time should still be very close to the original value but the details should
        // show that a rate change is in progress.
        assert_geq!(updated_utc.into_nanos(), (reference_before + OFFSET).into_nanos());
        assert_leq!(
            updated_utc.into_nanos(),
            (reference_after + OFFSET + filtered_delta_offset).into_nanos()
        );
        assert_geq!(details.reference_to_synthetic.rate.synthetic_ticks, 1000050);
        assert_eq!(details.reference_to_synthetic.rate.reference_ticks, 1000000);
        assert_geq!(details.last_rate_adjust_update_ticks, details.last_value_update_ticks);

        // After waiting for the first deferred update the clock should still be still at the
        // modified rate with a smaller error bound.
        assert!(executor.wake_next_timer().is_some());
        let _ = executor.run_until_stalled(&mut fut);
        // This stepwise execution of the control algorithm is sensitive to the changes
        // in task scheduling, and may need an arbitrary number of "stalled runs"
        // to complete with success. This additional stalled run is a consequence
        // of adding a new async `mpsc::Sender` for async commands.
        assert!(executor.wake_next_boot_timer().is_some());
        let _ = executor.run_until_stalled(&mut fut);

        let details2 = clock.get_details().unwrap();
        assert_geq!(details2.reference_to_synthetic.rate.synthetic_ticks, 1000050);
        assert_eq!(details2.reference_to_synthetic.rate.reference_ticks, 1000000);
        assert_lt!(details2.error_bounds, details.error_bounds);

        // After waiting for the next deferred update the clock should be back to the original rate
        // with an even smaller error bound.
        assert!(executor.wake_next_boot_timer().is_some());
        let _ = executor.run_until_stalled(&mut fut);
        let details3 = clock.get_details().unwrap();
        assert_eq!(details3.reference_to_synthetic.rate.synthetic_ticks, 1000000);
        assert_eq!(details3.reference_to_synthetic.rate.reference_ticks, 1000000);
        assert_lt!(details3.error_bounds, details2.error_bounds);

        // If we keep on waiting the error bound should keep increasing in the absence of updates.
        assert!(executor.wake_next_boot_timer().is_some());
        let _ = executor.run_until_stalled(&mut fut);
        let details4 = clock.get_details().unwrap();
        assert_eq!(details4.reference_to_synthetic, details3.reference_to_synthetic);
        assert_gt!(details4.error_bounds, details3.error_bounds);
        assert!(executor.wake_next_boot_timer().is_some());
        let _ = executor.run_until_stalled(&mut fut);
        let details5 = clock.get_details().unwrap();
        assert_eq!(details5.reference_to_synthetic, details3.reference_to_synthetic);
        assert_gt!(details5.error_bounds, details4.error_bounds);

        // Check that the correct diagnostic events were logged.
        diagnostics.assert_events(&[
            Event::TimeSourceStatus { role: TEST_ROLE, status: Status::Ok },
            Event::KalmanFilterUpdated {
                track: *TEST_TRACK,
                reference: reference - SAMPLE_SPACING,
                utc: UtcInstant::from_nanos((reference - SAMPLE_SPACING + OFFSET).into_nanos()),
                sqrt_covariance: STD_DEV,
            },
            Event::StartClock { track: *TEST_TRACK, source: *START_CLOCK_SOURCE },
            Event::KalmanFilterUpdated {
                track: *TEST_TRACK,
                reference,
                utc: UtcInstant::from_nanos(
                    (reference + OFFSET + filtered_delta_offset).into_nanos(),
                ),
                sqrt_covariance: zx::BootDuration::from_nanos(62225396),
            },
            Event::ClockCorrection {
                track: *TEST_TRACK,
                correction: UtcDuration::from_nanos(ANY_DURATION.into_nanos()),
                strategy: ClockCorrectionStrategy::MaxDurationSlew,
            },
            Event::UpdateClock { track: *TEST_TRACK, reason: ClockUpdateReason::BeginSlew },
            Event::TimeSourceStatus { role: TEST_ROLE, status: Status::Network },
            Event::UpdateClock { track: *TEST_TRACK, reason: ClockUpdateReason::ReduceError },
            Event::UpdateClock { track: *TEST_TRACK, reason: ClockUpdateReason::EndSlew },
            Event::UpdateClock { track: *TEST_TRACK, reason: ClockUpdateReason::IncreaseError },
            Event::UpdateClock { track: *TEST_TRACK, reason: ClockUpdateReason::IncreaseError },
        ]);
    }
}
