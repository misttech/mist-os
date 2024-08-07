// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Statistics and sample aggregation.

use derivative::Derivative;
use num::{Bounded, Num, NumCast, Zero};
use std::cmp;
use std::fmt::Debug;
use thiserror::Error;

use crate::experimental::series::buffer::BufferStrategy;
use crate::experimental::series::interpolation::Interpolation;
use crate::experimental::series::{BitSet, Counter, DataSemantic, Fill, Gauge, Sample, Sampler};

pub mod recipe {
    //! Type definitions and respellings of common or interesting statistic types.

    use crate::experimental::series::statistic::{ArithmeticMean, Sum, Transform};
    use crate::experimental::series::Gauge;

    pub type SumGauge<T> = Sum<Gauge<T>>;

    pub type ArithmeticMeanGauge<T> = ArithmeticMean<Gauge<T>>;
    pub type ArithmeticMeanTransform<T, A> = Transform<ArithmeticMeanGauge<T>, A>;
}

/// Statistic overflow error.
///
/// Describes overflow errors in statistic computations.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("overflow in statistic computation")]
pub struct OverflowError;

/// A `Sampler` that folds samples into a statistical aggregation.
pub trait Statistic: Clone + Fill<Self::Sample> {
    /// The type of data semantic associated with samples.
    type Semantic: DataSemantic<Sample = Self::Sample>;
    // This associated type is redundant, but provides a convenient way to spell the sample type in
    // bounds.
    /// The type of samples.
    type Sample: Clone;
    /// The type of the statistical aggregation.
    type Aggregation: Clone;

    /// Gets the statistical aggregation.
    ///
    /// Returns `None` if no aggregation is ready.
    ///
    /// This operation should clear out the samples used to compute the aggregation unless
    /// the Statistic is accumulative across sampling intervals (e.g. LatchMax).
    fn aggregation(&mut self) -> Option<Self::Aggregation>;
}

/// The associated data semantic type of a `Statistic`.
pub type Semantic<F> = <F as Statistic>::Semantic;

/// The associated aggregation type of a `Statistic`.
pub type Aggregation<F> = <F as Statistic>::Aggregation;

impl<F, P> BufferStrategy<F::Aggregation, P> for F
where
    F: Statistic,
    F::Semantic: BufferStrategy<F::Aggregation, P>,
    P: Interpolation,
{
    type Buffer = <F::Semantic as BufferStrategy<F::Aggregation, P>>::Buffer;
}

pub trait StatisticFor<P>: BufferStrategy<Self::Aggregation, P> + Statistic
where
    P: Interpolation<FillSample<Self> = Self::Sample>,
{
}

impl<F, P> StatisticFor<P> for F
where
    F: BufferStrategy<Self::Aggregation, P> + Statistic,
    P: Interpolation<FillSample<Self> = Self::Sample>,
{
}

/// Arithmetic mean statistic.
///
/// The arithmetic mean sums samples within their domain and computes the mean as a real number
/// represented using floating-point. For floating-point samples, `NaN`s are discarded and the sum
/// saturates to infinity without error.
///
/// This statistic is sensitive to overflow in the count of samples.
#[derive(Derivative)]
#[derivative(
    Clone(bound = "T::Sample: Clone,"),
    Debug(bound = "T::Sample: Debug,"),
    Default(bound = "T::Sample: Zero,")
)]
pub struct ArithmeticMean<T>
where
    T: DataSemantic,
{
    /// The sum of samples.
    #[derivative(Default(value = "Zero::zero()"))]
    sum: T::Sample,
    /// The count of samples.
    n: u64,
}

impl<T> ArithmeticMean<T>
where
    T: DataSemantic,
{
    fn increment(&mut self, m: u64) -> Result<(), OverflowError> {
        // TODO(https://fxbug.dev/351848566): Handle overflow before returning OverflowError
        self.n.checked_add(m).inspect(|sum| self.n = *sum).map(|_| ()).ok_or(OverflowError)
    }
}

impl<T> Fill<T::Sample> for ArithmeticMean<T>
where
    Self: Sampler<T::Sample, Error = OverflowError>,
    T: DataSemantic,
    T::Sample: Num + NumCast,
{
    fn fill(&mut self, sample: T::Sample, n: usize) -> Result<(), Self::Error> {
        Ok(match num::cast::<_, T::Sample>(n) {
            Some(m) if n > 0 => {
                self.fold(sample * m)?;
                self.increment((n as u64) - 1)?;
            }
            _ => {
                for _ in 0..n {
                    self.fold(sample.clone())?;
                }
            }
        })
    }
}

impl Sampler<Sample<Gauge<f32>>> for ArithmeticMean<Gauge<f32>> {
    type Error = OverflowError;

    fn fold(&mut self, sample: Sample<Gauge<f32>>) -> Result<(), Self::Error> {
        // Discard `NaN` terms and avoid some floating-point exceptions.
        self.sum = match sample {
            _ if sample.is_nan() => self.sum,
            sample if sample.is_infinite() => sample,
            // Because neither term is `NaN` and `sample` is finite, this saturates at negative or
            // positive infinity.
            sample => self.sum + sample,
        };
        self.increment(1)
    }
}

impl Statistic for ArithmeticMean<Gauge<f32>> {
    type Semantic = Gauge<f32>;
    type Sample = Sample<Gauge<f32>>;
    type Aggregation = f32;

    fn aggregation(&mut self) -> Option<Self::Aggregation> {
        // This is lossy and lossiness correlates to the magnitude of `n`. See details of
        // `u64 as f32` casts.
        let aggregation = (self.n > 0).then(|| self.sum / (self.n as f32));
        *self = Default::default();
        aggregation
    }
}

/// Sum statistic.
///
/// The sum directly computes the aggregation in the domain of samples.
///
/// This statistic is sensitive to overflow in the sum of samples.
#[derive(Derivative)]
#[derivative(
    Clone(bound = "T::Sample: Clone,"),
    Debug(bound = "T::Sample: Debug,"),
    Default(bound = "T::Sample: Zero,")
)]
pub struct Sum<T>
where
    T: DataSemantic,
{
    /// The sum of samples.
    #[derivative(Default(value = "Zero::zero()"))]
    sum: T::Sample,
}

impl<T> Fill<T::Sample> for Sum<T>
where
    Self: Sampler<T::Sample>,
    T: DataSemantic,
    T::Sample: Num + NumCast,
{
    fn fill(&mut self, sample: T::Sample, n: usize) -> Result<(), Self::Error> {
        if let Some(n) = num::cast::<_, T::Sample>(n) {
            self.fold(sample * n)
        } else {
            Ok(for _ in 0..n {
                self.fold(sample.clone())?;
            })
        }
    }
}

impl Sampler<Sample<Gauge<u64>>> for Sum<Gauge<u64>> {
    type Error = OverflowError;

    fn fold(&mut self, sample: Sample<Gauge<u64>>) -> Result<(), Self::Error> {
        // TODO(https://fxbug.dev/351848566): Handle overflow before returning OverflowError
        self.sum.checked_add(sample).inspect(|sum| self.sum = *sum).map(|_| ()).ok_or(OverflowError)
    }
}

impl Statistic for Sum<Gauge<u64>> {
    type Semantic = Gauge<u64>;
    type Sample = Sample<Gauge<u64>>;
    type Aggregation = u64;

    fn aggregation(&mut self) -> Option<Self::Aggregation> {
        let sum = self.sum;
        *self = Default::default();
        Some(sum)
    }
}

/// Max statistic.
#[derive(Derivative)]
#[derivative(
    Clone(bound = "T::Sample: Clone,"),
    Debug(bound = "T::Sample: Debug,"),
    Default(bound = "T::Sample: Zero,")
)]
pub struct Max<T>
where
    T: DataSemantic,
{
    /// The max of samples.
    #[derivative(Default(value = "Zero::zero()"))]
    max: T::Sample,
}

impl<T> Fill<T::Sample> for Max<T>
where
    Self: Sampler<T::Sample, Error = OverflowError>,
    T: DataSemantic,
    T::Sample: Num + NumCast,
{
    fn fill(&mut self, sample: T::Sample, _n: usize) -> Result<(), Self::Error> {
        self.fold(sample)
    }
}

impl Sampler<Sample<Gauge<u64>>> for Max<Gauge<u64>> {
    type Error = OverflowError;

    fn fold(&mut self, sample: Sample<Gauge<u64>>) -> Result<(), Self::Error> {
        self.max = std::cmp::max(self.max, sample);
        Ok(())
    }
}

impl Statistic for Max<Gauge<u64>> {
    type Semantic = Gauge<u64>;
    type Sample = Sample<Gauge<u64>>;
    type Aggregation = u64;

    fn aggregation(&mut self) -> Option<Self::Aggregation> {
        let max = self.max;
        *self = Default::default();
        Some(max)
    }
}

#[derive(Derivative)]
#[derivative(
    Clone(bound = "T::Sample: Clone,"),
    Debug(bound = "T::Sample: Debug,"),
    Default(bound = "T::Sample: Zero,")
)]
pub struct Union<T>
where
    T: DataSemantic,
{
    #[derivative(Default(value = "Zero::zero()"))]
    value: T::Sample,
}

impl<T> Fill<T::Sample> for Union<T>
where
    Self: Sampler<T::Sample, Error = OverflowError>,
    T: DataSemantic,
    T::Sample: Num + NumCast,
{
    fn fill(&mut self, sample: T::Sample, _n: usize) -> Result<(), Self::Error> {
        self.fold(sample)
    }
}

impl Sampler<Sample<BitSet<u64>>> for Union<BitSet<u64>> {
    type Error = OverflowError;

    fn fold(&mut self, sample: Sample<BitSet<u64>>) -> Result<(), Self::Error> {
        self.value = self.value | sample;
        Ok(())
    }
}

impl Statistic for Union<BitSet<u64>> {
    type Semantic = BitSet<u64>;
    type Sample = Sample<BitSet<u64>>;
    type Aggregation = u64;

    fn aggregation(&mut self) -> Option<Self::Aggregation> {
        let value = self.value;
        *self = Default::default();
        Some(value)
    }
}

/// Maximum statistic that sums non-monotonic samples into the maximum.
///
/// This statistic is sensitive to overflow in the sum of samples with the non-monotonic sum.
#[derive(Derivative)]
#[derivative(
    Clone(bound = "T::Sample: Clone,"),
    Debug(bound = "T::Sample: Debug,"),
    Default(bound = "T::Sample: Bounded + Zero,")
)]
pub struct LatchMax<T>
where
    T: DataSemantic,
{
    /// The last observed sample.
    last: Option<T::Sample>,
    /// The maximum of samples and the non-monotonic sum.
    #[derivative(Default(value = "Bounded::min_value()"))]
    max: T::Sample,
    /// The sum of non-monotonic samples.
    #[derivative(Default(value = "Zero::zero()"))]
    sum: T::Sample,
}

impl<T> Fill<T::Sample> for LatchMax<T>
where
    Self: Sampler<T::Sample>,
    T: DataSemantic,
{
    fn fill(&mut self, sample: T::Sample, _: usize) -> Result<(), Self::Error> {
        self.fold(sample)
    }
}

impl Sampler<Sample<Counter<u64>>> for LatchMax<Counter<u64>> {
    type Error = OverflowError;

    fn fold(&mut self, sample: Sample<Counter<u64>>) -> Result<(), Self::Error> {
        // TODO(https://fxbug.dev/351848566): Handle overflow before returning OverflowError
        match self.last {
            Some(last) if sample < last => {
                self.sum = self.sum.checked_add(last).ok_or(OverflowError)?;
            }
            _ => {}
        }
        self.last = Some(sample);
        self.max = cmp::max(self.max, sample.checked_add(self.sum).ok_or(OverflowError)?);
        Ok(())
    }
}

impl Statistic for LatchMax<Counter<u64>> {
    type Semantic = Counter<u64>;
    type Sample = Sample<Counter<u64>>;
    type Aggregation = u64;

    fn aggregation(&mut self) -> Option<Self::Aggregation> {
        Some(self.max)
    }
}

/// Post-aggregation transform.
///
/// A post-aggregation composes a [`Statistic`] and arbitrarily maps its aggregation. For example,
/// a transform may discretize the continuous aggregation of a statistic.
///
/// [`Statistic`]: crate::experimental::series::statistic::Statistic
#[derive(Clone, Copy, Debug)]
pub struct PostAggregation<F, R> {
    statistic: F,
    transform: R,
}

/// A post-aggregation of a function pointer.
///
/// This type definition describes a stateless and pure transform with a type name that can be
/// spelled in any context (i.e., no closure or other unnameable types occur).
pub type Transform<F, A> = PostAggregation<F, fn(Aggregation<F>) -> A>;

impl<F, R> PostAggregation<F, R>
where
    F: Default,
{
    /// Constructs a `PostAggregation` from a transform function.
    ///
    /// The default statistic is composed.
    pub fn from_transform(transform: R) -> Self {
        PostAggregation { statistic: F::default(), transform }
    }
}

impl<T, F, R> Fill<T> for PostAggregation<F, R>
where
    F: Fill<T>,
{
    fn fill(&mut self, sample: T, n: usize) -> Result<(), Self::Error> {
        self.statistic.fill(sample, n)
    }
}

impl<T, F, R> Sampler<T> for PostAggregation<F, R>
where
    F: Sampler<T>,
{
    type Error = F::Error;

    fn fold(&mut self, sample: T) -> Result<(), Self::Error> {
        self.statistic.fold(sample)
    }
}

impl<A, F, R> Statistic for PostAggregation<F, R>
where
    F: Statistic,
    R: Clone + Fn(F::Aggregation) -> A,
    A: Clone,
{
    type Semantic = F::Semantic;
    type Sample = F::Sample;
    type Aggregation = A;

    fn aggregation(&mut self) -> Option<Self::Aggregation> {
        self.statistic.aggregation().map(|aggregation| (self.transform)(aggregation))
    }
}

#[cfg(test)]
mod tests {
    use crate::experimental::series::statistic::{
        ArithmeticMean, LatchMax, Max, OverflowError, PostAggregation, Statistic, Sum, Union,
    };
    use crate::experimental::series::{BitSet, Counter, Fill, Gauge, Sampler};

    #[test]
    fn arithmetic_mean_gauge_aggregation() {
        let mut mean = ArithmeticMean::<Gauge<f32>>::default();
        mean.fold(1.0).unwrap();
        mean.fold(1.0).unwrap();
        mean.fold(1.0).unwrap();
        let aggregation = mean.aggregation().unwrap();
        assert!(aggregation > 0.99 && aggregation < 1.01); // ~ 1.0

        let mut mean = ArithmeticMean::<Gauge<f32>>::default();
        mean.fold(0.0).unwrap();
        mean.fold(1.0).unwrap();
        mean.fold(2.0).unwrap();
        let aggregation = mean.aggregation().unwrap();
        assert!(aggregation > 0.99 && aggregation < 1.01); // ~ 1.0
    }

    #[test]
    fn arithmetic_mean_gauge_aggregation_fill() {
        let mut mean = ArithmeticMean::<Gauge<f32>>::default();
        mean.fill(1.0, 1000).unwrap();
        let aggregation = mean.aggregation().unwrap();
        assert!(aggregation > 0.99 && aggregation < 1.01); // ~ 1.0
    }

    #[test]
    fn arithmetic_mean_gauge_overflow() {
        let mut mean = ArithmeticMean::<Gauge<f32>> { sum: 1.0, n: u64::MAX };
        let result = mean.fold(1.0);
        assert_eq!(result, Err(OverflowError));
    }

    #[test]
    fn sum_gauge_aggregation() {
        let mut sum = Sum::<Gauge<u64>>::default();
        sum.fold(1).unwrap();
        sum.fold(1).unwrap();
        sum.fold(1).unwrap();
        let aggregation = sum.aggregation().unwrap();
        assert_eq!(aggregation, 3);

        let mut sum = Sum::<Gauge<u64>>::default();
        sum.fold(0).unwrap();
        sum.fold(1).unwrap();
        sum.fold(2).unwrap();
        let aggregation = sum.aggregation().unwrap();
        assert_eq!(aggregation, 3);
    }

    #[test]
    fn sum_gauge_aggregation_fill() {
        let mut sum = Sum::<Gauge<u64>>::default();
        sum.fill(10, 1000).unwrap();
        let aggregation = sum.aggregation().unwrap();
        assert_eq!(aggregation, 10_000);
    }

    #[test]
    fn sum_gauge_overflow() {
        let mut sum = Sum::<Gauge<u64>> { sum: u64::MAX };
        let result = sum.fold(1);
        assert_eq!(result, Err(OverflowError));
    }

    #[test]
    fn max_gauge_aggregation() {
        let mut max = Max::<Gauge<u64>>::default();
        max.fold(0).unwrap();
        max.fold(1337).unwrap();
        max.fold(42).unwrap();
        let aggregation = max.aggregation().unwrap();
        assert_eq!(aggregation, 1337);
    }

    #[test]
    fn max_gauge_aggregation_fill() {
        let mut max = Max::<Gauge<u64>>::default();
        max.fill(42, 1000).unwrap();
        let aggregation = max.aggregation().unwrap();
        assert_eq!(aggregation, 42);
    }

    #[test]
    fn union_bitset_aggregation() {
        let mut value = Union::<BitSet<u64>>::default();
        value.fold(1 << 1).unwrap();
        value.fold(1 << 3).unwrap();
        value.fold(1 << 5).unwrap();
        let aggregation = value.aggregation().unwrap();
        assert_eq!(aggregation, 0b101010);
    }

    #[test]
    fn union_bitset_aggregation_fill() {
        let mut value = Union::<BitSet<u64>>::default();
        value.fill(1 << 2, 1000).unwrap();
        let aggregation = value.aggregation().unwrap();
        assert_eq!(aggregation, 0b100);
    }

    #[test]
    fn latch_max_counter_aggregation() {
        let mut max = LatchMax::<Counter<u64>>::default();
        max.fold(1).unwrap();
        max.fold(1).unwrap();
        max.fold(1).unwrap();
        let aggregation = max.aggregation().unwrap();
        assert_eq!(aggregation, 1);

        let mut max = LatchMax::<Counter<u64>>::default();
        max.fold(0).unwrap();
        max.fold(1).unwrap();
        max.fold(2).unwrap();
        let aggregation = max.aggregation().unwrap();
        assert_eq!(aggregation, 2);

        let mut max = LatchMax::<Counter<u64>>::default();
        max.fold(1).unwrap();
        max.fold(5).unwrap();
        max.fold(6).unwrap();
        max.fold(2).unwrap(); // Non-monotonic sum is six.
        max.fold(9).unwrap();
        max.fold(3).unwrap(); // Non-monotonic sum is 15 (6 + 9).
        max.fold(5).unwrap();
        let aggregation = max.aggregation().unwrap();
        assert_eq!(aggregation, 20);
    }

    #[test]
    fn latch_max_counter_aggregation_fill() {
        let mut max = LatchMax::<Counter<u64>>::default();
        max.fill(10, 1000).unwrap();
        let aggregation = max.aggregation().unwrap();
        assert_eq!(aggregation, 10);
    }

    #[test]
    fn latch_max_counter_overflow() {
        let mut max = LatchMax::<Counter<u64>>::default();
        max.fold(1).unwrap();
        max.fold(0).unwrap(); // Non-monotonic sum is one.
        let result = max.fold(u64::MAX); // Non-monotonic sum of one is added to `u64::MAX`.
        assert_eq!(result, Err(OverflowError));
    }

    #[test]
    fn post_sum_gauge_aggregation() {
        let mut sum = PostAggregation::<Sum<Gauge<u64>>, _>::from_transform(|sum| sum + 1);
        sum.fold(0).unwrap();
        sum.fold(1).unwrap();
        sum.fold(2).unwrap();
        let aggregation = sum.aggregation().unwrap();
        assert_eq!(aggregation, 4);
    }
}
