// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Round-robin multi-resolution time series.

mod buffer;
mod interval;

pub mod interpolation;
pub mod statistic;

use derivative::Derivative;
use num::{Num, Unsigned};
use std::convert::Infallible;
use std::fmt::Debug;
use std::io;
use std::marker::PhantomData;
use thiserror::Error;

use crate::experimental::clock::{
    MonotonicityError, ObservationTime, Tick, TimedSample, Timestamp,
};
use crate::experimental::series::buffer::{Buffer, BufferStrategy, DeltaSimple8bRle, RingBuffer};
use crate::experimental::series::interpolation::{
    Interpolation, InterpolationFor, InterpolationState, LastAggregation, LastSample,
};
use crate::experimental::series::statistic::{OverflowError, PostAggregation, Statistic};
use crate::experimental::Vec1;

pub use crate::experimental::series::interval::{SamplingInterval, SamplingProfile};

/// Sample folding error.
///
/// Describes errors that occur when folding a sample into a [`Sampler`].
///
/// [`Sampler`]: crate::experimental::series::Sampler
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FoldError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Monotonicity(#[from] MonotonicityError),
    #[error(transparent)]
    Overflow(#[from] OverflowError),
}

impl From<Infallible> for FoldError {
    fn from(_: Infallible) -> Self {
        unreachable!()
    }
}

/// A type that folds samples into an aggregation or some other state.
pub trait Sampler<T> {
    /// The type of error that can occur when [folding samples][`Sampler::fold`] into the sampler.
    ///
    /// [`Sampler::fold`]: crate::experimental::series::Sampler::fold
    type Error;

    fn fold(&mut self, sample: T) -> Result<(), Self::Error>;
}

/// A [`Sampler`] that can efficiently fold zero or more of a particular sample.
pub trait Fill<T>: Sampler<T> {
    fn fill(&mut self, sample: T, n: usize) -> Result<(), Self::Error>;
}

/// A buffered round-robin sampler over [timed samples][`TimedSample`] (e.g., a [`TimeMatrix`]).
///
/// Round-robin samplers aggregate samples into buffered time series and produce a serialized
/// buffer of aggregations per series.
///
/// [`TimedSample`]: crate::experimental::clock::TimedSample
/// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
pub trait RoundRobinSampler<T>: Sampler<TimedSample<T>, Error = FoldError> {
    /// Interpolates samples to the given timestamp and gets the serialized aggregation buffers.
    ///
    /// This function queries the aggregations of the series. Typically, the timestamp is the
    /// current time.
    fn interpolate(
        &mut self,
        timestamp: impl Into<Timestamp>,
    ) -> Result<Vec1<SerializedBuffer>, Self::Error>;
}

/// A type constructor that describes the semantics of data sampled from a column in an event.
///
/// Data semantics determine how statistics are interpreted and time series are aggregated and
/// buffered.
pub trait DataSemantic {
    /// The type of sample data associated with the data semantic.
    type Sample: Clone;
}

/// The associated sample type of a `DataSemantic`.
pub type Sample<T> = <T as DataSemantic>::Sample;

/// A continually increasing value.
///
/// Counters are analogous to an odometer in a vehicle.
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Counter<T>(PhantomData<fn() -> T>, Infallible);

impl<T> BufferStrategy<u64, LastAggregation> for Counter<T> {
    type Buffer = DeltaSimple8bRle;
}

impl<T> BufferStrategy<u64, LastSample> for Counter<T> {
    type Buffer = DeltaSimple8bRle;
}

impl<T> DataSemantic for Counter<T>
where
    T: Clone + Into<u64> + Unsigned,
{
    type Sample = T;
}

/// A fluctuating value.
///
/// Gauges are analogous to a speedometer in a vehicle.
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Gauge<T>(PhantomData<fn() -> T>, Infallible);

// Gauge semantics forward this implementation to the aggregation type.
impl<T, A, P> BufferStrategy<A, P> for Gauge<T>
where
    A: BufferStrategy<A, P>,
    P: Interpolation,
{
    type Buffer = A::Buffer;
}

impl<T> DataSemantic for Gauge<T>
where
    T: Clone + Num,
{
    type Sample = T;
}

/// A serialized buffer of aggregations.
#[derive(Clone, Debug)]
pub struct SerializedBuffer {
    interval: SamplingInterval,
    data: Vec<u8>,
}

impl SerializedBuffer {
    /// Gets the sampling interval for the aggregations in the buffer.
    pub fn interval(&self) -> &SamplingInterval {
        &self.interval
    }

    /// Gets the serialized data.
    pub fn data(&self) -> &[u8] {
        self.data.as_slice()
    }
}

impl AsRef<[u8]> for SerializedBuffer {
    fn as_ref(&self) -> &[u8] {
        self.data.as_slice()
    }
}

/// An unbuffered statistical time series specification.
///
/// This type samples and interpolates timed data and produces aggregations per its statistic and
/// sampling interval. It is a specification insofar that it does **not** buffer the series of
/// aggregations.
#[derive(Clone, Debug)]
struct TimeSeries<F>
where
    F: Statistic,
{
    interval: SamplingInterval,
    statistic: F,
}

impl<F> TimeSeries<F>
where
    F: Statistic,
{
    pub fn new(interval: SamplingInterval) -> Self
    where
        F: Default,
    {
        TimeSeries { interval, statistic: F::default() }
    }

    pub const fn with_statistic(interval: SamplingInterval, statistic: F) -> Self {
        TimeSeries { interval, statistic }
    }

    /// Folds interpolations for intervals intersected by the given [`Tick`] and gets the
    /// aggregations.
    ///
    /// The returned iterator performs the computation and so it must be consumed to change the
    /// state of the statistic.
    ///
    /// [`Tick`]: crate::experimental::clock::Tick
    #[must_use]
    fn interpolate_and_get_aggregations<'i, P>(
        &'i mut self,
        interpolation: &'i mut P,
        tick: Tick,
    ) -> impl 'i + Iterator<Item = Result<F::Aggregation, F::Error>>
    where
        P: InterpolationState<F::Aggregation, FillSample = F::Sample>,
    {
        self.interval.fold_and_get_expirations(tick, PhantomData::<F::Sample>).flat_map(
            move |expiration| {
                expiration
                    .interpolate_and_get_aggregation(&mut self.statistic, interpolation)
                    .transpose()
            },
        )
    }

    /// Folds the given sample and interpolations for intervals intersected by the given [`Tick`]
    /// and gets the aggregations.
    ///
    /// The returned iterator performs the computation and so it must be consumed to change the
    /// state of the statistic.
    ///
    /// [`Tick`]: crate::experimental::clock::Tick
    #[must_use]
    fn fold_and_get_aggregations<'i, P>(
        &'i mut self,
        interpolation: &'i mut P,
        tick: Tick,
        sample: F::Sample,
    ) -> impl 'i + Iterator<Item = Result<F::Aggregation, F::Error>>
    where
        P: InterpolationState<F::Aggregation, FillSample = F::Sample>,
    {
        self.interval.fold_and_get_expirations(tick, sample).flat_map(move |expiration| {
            expiration.fold_and_get_aggregation(&mut self.statistic, interpolation).transpose()
        })
    }

    /// Gets the sampling interval of the series.
    pub fn interval(&self) -> &SamplingInterval {
        &self.interval
    }
}

impl<F, R, A> TimeSeries<PostAggregation<F, R>>
where
    F: Default + Statistic,
    R: Clone + Fn(F::Aggregation) -> A,
    A: Clone,
{
    pub fn with_transform(interval: SamplingInterval, transform: R) -> Self {
        TimeSeries { interval, statistic: PostAggregation::from_transform(transform) }
    }
}

/// A buffered round-robin statistical time series.
///
/// This type composes a [`TimeSeries`] with a round-robin buffer of aggregations and interpolation
/// state. Aggregations produced by the time series when sampling or interpolating are pushed into
/// the buffer.
#[derive(Derivative)]
#[derivative(
    Clone(bound = "F: Clone, Buffer<F, P>: Clone, P::State<F>: Clone,"),
    Debug(bound = "F: Debug,
                   F::Sample: Debug,
                   F::Aggregation: Debug,
                   Buffer<F, P>: Debug,
                   P::State<F>: Debug,")
)]
struct BufferedTimeSeries<F, P>
where
    F: BufferStrategy<F::Aggregation, P> + Statistic,
    P: Interpolation<FillSample<F> = F::Sample>,
{
    buffer: Buffer<F, P>,
    interpolation: P::State<F>,
    series: TimeSeries<F>,
}

impl<F, P> BufferedTimeSeries<F, P>
where
    F: BufferStrategy<F::Aggregation, P> + Statistic,
    P: Interpolation<FillSample<F> = F::Sample>,
{
    pub fn new(interpolation: P::State<F>, series: TimeSeries<F>) -> Self {
        let buffer = F::buffer(&series.interval);
        BufferedTimeSeries { buffer, interpolation, series }
    }

    /// Folds interpolations for intervals intersected by the given [`Tick`] and buffers the
    /// aggregations.
    ///
    /// # Errors
    ///
    /// Returns an error if sampling fails.
    ///
    /// [`Tick`]: crate::experimental::clock::Tick
    fn interpolate(&mut self, tick: Tick) -> Result<(), F::Error> {
        for aggregation in
            self.series.interpolate_and_get_aggregations(&mut self.interpolation, tick)
        {
            self.buffer.push(aggregation?);
        }
        Ok(())
    }

    /// Folds the given sample and interpolations for intervals intersected by the given [`Tick`]
    /// and buffers the aggregations.
    ///
    /// # Errors
    ///
    /// Returns an error if sampling fails.
    ///
    /// [`Tick`]: crate::experimental::clock::Tick
    fn fold(&mut self, tick: Tick, sample: F::Sample) -> Result<(), F::Error> {
        for aggregation in
            self.series.fold_and_get_aggregations(&mut self.interpolation, tick, sample)
        {
            self.buffer.push(aggregation?);
        }
        Ok(())
    }

    pub fn serialize_and_get_buffer(&self) -> io::Result<SerializedBuffer> {
        let mut data = vec![];
        self.buffer.serialize(&mut data)?;
        Ok(SerializedBuffer { interval: *self.series.interval(), data })
    }
}

/// One or more statistical round-robin time series.
///
/// A time matrix is a round-robin multi-resolution time series that samples and interpolates timed
/// data, computes statistical aggregations for elapsed [sampling intervals][`SamplingInterval`],
/// and buffers those aggregations. The sample data, statistic, and interpolation of series in a
/// time matrix must be the same, but the sampling intervals can and should differ.
#[derive(Derivative)]
#[derivative(
    Clone(bound = "F: Clone, Buffer<F, P>: Clone, P::State<F>: Clone,"),
    Debug(bound = "F: Debug,
                   F::Sample: Debug,
                   F::Aggregation: Debug,
                   Buffer<F, P>: Debug,
                   P::State<F>: Debug,")
)]
pub struct TimeMatrix<F, P>
where
    F: BufferStrategy<F::Aggregation, P> + Statistic,
    P: Interpolation<FillSample<F> = F::Sample>,
{
    last: ObservationTime,
    buffers: Vec1<BufferedTimeSeries<F, P>>,
}

impl<F, P> TimeMatrix<F, P>
where
    F: BufferStrategy<F::Aggregation, P> + Statistic,
    P: Interpolation<FillSample<F> = F::Sample>,
{
    fn from_series_with<Q>(series: impl Into<Vec1<TimeSeries<F>>>, mut interpolation: Q) -> Self
    where
        Q: FnMut() -> P::State<F>,
    {
        let buffers =
            series.into().map_into(|series| BufferedTimeSeries::new((interpolation)(), series));
        TimeMatrix { last: ObservationTime::default(), buffers }
    }

    /// Constructs a time matrix with the given sampling profile and interpolation.
    ///
    /// Statistics are default initialized.
    pub fn new(profile: impl Into<SamplingProfile>, interpolation: P::State<F>) -> Self
    where
        F: Default,
    {
        let sampling_intervals = F::sampling_intervals(&profile.into());
        TimeMatrix::from_series_with(sampling_intervals.map_into(TimeSeries::new), || {
            interpolation.clone()
        })
    }

    /// Constructs a time matrix with the given statistic.
    pub fn with_statistic(
        profile: impl Into<SamplingProfile>,
        interpolation: P::State<F>,
        statistic: F,
    ) -> Self {
        let sampling_intervals = F::sampling_intervals(&profile.into());
        TimeMatrix::from_series_with(
            sampling_intervals
                .map_into(|window| TimeSeries::with_statistic(window, statistic.clone())),
            || interpolation.clone(),
        )
    }

    /// Folds interpolations and gets the aggregation buffers.
    pub fn interpolate_and_get_buffers(
        &mut self,
        timestamp: Timestamp,
    ) -> Result<Vec1<SerializedBuffer>, FoldError>
    where
        FoldError: From<F::Error>,
    {
        let tick = self.last.tick(timestamp, false)?;
        self.buffers.try_map_mut(|buffer| {
            buffer.interpolate(tick)?;
            buffer.serialize_and_get_buffer().map_err(From::from)
        })
    }

    /// Folds the given sample and interpolations and gets the aggregation buffers.
    ///
    /// To fold a sample without serializing buffers, use [`Sampler::fold`].
    ///
    /// [`Sampler::fold`]: crate::experimental::series::Sampler::fold
    pub fn fold_and_get_buffers(
        &mut self,
        sample: TimedSample<F::Sample>,
    ) -> Result<Vec1<SerializedBuffer>, FoldError>
    where
        FoldError: From<F::Error>,
    {
        self.fold(sample)?;
        self.buffers.try_map_ref(BufferedTimeSeries::serialize_and_get_buffer).map_err(From::from)
    }
}

impl<F, R, P, A> TimeMatrix<PostAggregation<F, R>, P>
where
    PostAggregation<F, R>: BufferStrategy<A, P>,
    F: Default + Statistic,
    R: Clone + Fn(F::Aggregation) -> A,
    P: InterpolationFor<PostAggregation<F, R>>,
    A: Clone,
{
    /// Constructs a time matrix with the default statistic and given transform for
    /// post-aggregation.
    pub fn with_transform(
        profile: impl Into<SamplingProfile>,
        interpolation: P::State<PostAggregation<F, R>>,
        transform: R,
    ) -> Self
    where
        R: Clone,
    {
        let sampling_intervals = PostAggregation::<F, R>::sampling_intervals(&profile.into());
        TimeMatrix::from_series_with(
            sampling_intervals
                .map_into(|window| TimeSeries::with_transform(window, transform.clone())),
            || interpolation.clone(),
        )
    }
}

impl<F, P> Default for TimeMatrix<F, P>
where
    F: BufferStrategy<F::Aggregation, P> + Default + Statistic,
    P: Interpolation<FillSample<F> = F::Sample>,
    P::State<F>: Default,
{
    fn default() -> Self {
        TimeMatrix::new(SamplingProfile::default(), P::State::default())
    }
}

impl<F, P> RoundRobinSampler<F::Sample> for TimeMatrix<F, P>
where
    FoldError: From<F::Error>,
    F: BufferStrategy<F::Aggregation, P> + Statistic,
    P: Interpolation<FillSample<F> = F::Sample>,
{
    fn interpolate(
        &mut self,
        timestamp: impl Into<Timestamp>,
    ) -> Result<Vec1<SerializedBuffer>, FoldError> {
        self.interpolate_and_get_buffers(timestamp.into())
    }
}

impl<F, P> Sampler<TimedSample<F::Sample>> for TimeMatrix<F, P>
where
    FoldError: From<F::Error>,
    F: BufferStrategy<F::Aggregation, P> + Statistic,
    P: Interpolation<FillSample<F> = F::Sample>,
{
    type Error = FoldError;

    fn fold(&mut self, timed: TimedSample<F::Sample>) -> Result<(), Self::Error> {
        let (timestamp, sample) = timed.into();
        let tick = self.last.tick(timestamp, true)?;
        Ok(for buffer in self.buffers.iter_mut() {
            buffer.fold(tick, sample.clone())?;
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::experimental::clock::{TimedSample, Timestamp};
    use crate::experimental::series::interpolation::{Constant, LastAggregation, LastSample};
    use crate::experimental::series::statistic::{
        ArithmeticMean, LatchMax, Max, PostAggregation, Sum, Transform,
    };
    use crate::experimental::series::{
        Counter, Gauge, RoundRobinSampler, SamplingProfile, TimeMatrix,
    };
    use fuchsia_async as fasync;

    fn fold_and_interpolate_f64(sampler: &mut impl RoundRobinSampler<f64>) {
        sampler.fold(TimedSample::now(0.0)).unwrap();
        sampler.fold(TimedSample::now(1.0)).unwrap();
        sampler.fold(TimedSample::now(2.0)).unwrap();
        let _buffers = sampler.interpolate(Timestamp::now()).unwrap();
    }

    // This test is considered successful as long as it builds.
    // It's to ensure that `TimeMatrix` types operate as expected on a type level.
    // TODO(https://fxbug.dev/356218503): Remove this unit test once we have unit tests that
    // actually verify the output of TimeMatrix.
    #[test]
    fn static_test_define_time_matrix() {
        let _exec = fasync::TestExecutor::new_with_fake_time();
        type MeanGauge<T> = ArithmeticMean<Gauge<T>>;
        type MeanGaugeTransform<T, U> = Transform<MeanGauge<T>, U>;

        // Arithmetic mean time matrices.
        let _ = TimeMatrix::<MeanGauge<f64>, Constant>::default();
        let _ = TimeMatrix::<MeanGauge<f64>, LastSample>::new(
            SamplingProfile::Balanced,
            LastSample::or(0.0f64),
        );
        let _ = TimeMatrix::<_, Constant>::with_statistic(
            SamplingProfile::Balanced,
            Constant::default(),
            MeanGauge::<f64>::default(),
        );

        // Discrete arithmetic mean time matrices.
        let mut matrix = TimeMatrix::<MeanGaugeTransform<f64, i64>, LastSample>::with_transform(
            SamplingProfile::Granular,
            LastSample::or(0.0f64),
            |aggregation| aggregation.ceil() as i64,
        );
        fold_and_interpolate_f64(&mut matrix);
        // This time matrix is constructed verbosely with no ad-hoc type definitions nor ergonomic
        // constructors. This is as raw as it gets.
        let mut matrix = TimeMatrix::<_, Constant>::with_statistic(
            SamplingProfile::default(),
            Constant::default(),
            PostAggregation::<ArithmeticMean<Gauge<f64>>, _>::from_transform(|aggregation: f64| {
                aggregation.ceil() as i64
            }),
        );
        fold_and_interpolate_f64(&mut matrix);
    }

    // This test is considered successful as long as it builds.
    // It's to ensure that `TimeMatrix` types operate as expected on a type level.
    // TODO(https://fxbug.dev/356218503): Remove this unit test once we have unit tests that
    // actually verify the output of TimeMatrix.
    #[test]
    fn static_test_supported_statistic_and_interpolation_combinations() {
        let _exec = fasync::TestExecutor::new_with_fake_time();
        let _ = TimeMatrix::<LatchMax<Counter<u64>>, LastSample>::default();
        let _ = TimeMatrix::<LatchMax<Counter<u64>>, LastAggregation>::default();
        let _ = TimeMatrix::<ArithmeticMean<Gauge<f64>>, Constant>::default();
        let _ = TimeMatrix::<ArithmeticMean<Gauge<f64>>, LastSample>::default();
        let _ = TimeMatrix::<ArithmeticMean<Gauge<f64>>, LastAggregation>::default();
        let _ = TimeMatrix::<Sum<Gauge<u64>>, Constant>::default();
        let _ = TimeMatrix::<Sum<Gauge<u64>>, LastSample>::default();
        let _ = TimeMatrix::<Sum<Gauge<u64>>, LastAggregation>::default();
        let _ = TimeMatrix::<Max<Gauge<u64>>, Constant>::default();
        let _ = TimeMatrix::<Max<Gauge<u64>>, LastSample>::default();
        let _ = TimeMatrix::<Max<Gauge<u64>>, LastAggregation>::default();
    }
}
