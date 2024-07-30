// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Sample and aggregation interpolation.

use std::mem;

use crate::experimental::series::statistic::Statistic;

/// A type constructor for an interpolation over the samples or aggregations of a [`Statistic`].
///
/// [`Statistic`]: crate::experimental::series::statistic::Statistic
pub trait Interpolation {
    /// The type of fill (interpolated) samples computed by the interpolation.
    type FillSample<F>: Clone
    where
        F: Statistic;
    /// The state (output type) of the interpolation type constructor.
    type State<F>: InterpolationState<F::Aggregation, FillSample = Self::FillSample<F>>
    where
        F: Statistic;
}

/// An interpolation type constructor with the same sample type as the given [`Statistic`] type.
///
/// [`Statistic`]: crate::experimental::series::statistic::Statistic
pub trait InterpolationFor<F>: Interpolation<FillSample<F> = F::Sample>
where
    F: Statistic,
{
}

impl<P, F> InterpolationFor<F> for P
where
    F: Statistic,
    P: Interpolation<FillSample<F> = F::Sample>,
{
}

/// A type constructor for an interpolation that can be suspended.
///
/// A suspended interpolation can produce different fill samples while the system is in a power
/// saving mode of operation, etc.
pub trait Suspend: Interpolation {
    type Suspended<F>: InterpolationState<F::Aggregation, FillSample = Self::FillSample<F>>
    where
        F: Statistic;
}

/// An interpolation over samples and statistical aggregations.
pub trait InterpolationState<A>: Clone {
    /// The type of fill (interpolated) samples computed by the interpolation.
    type FillSample: Clone;

    /// Gets the fill (interpolated) sample of the interpolation.
    fn sample(&self) -> Self::FillSample;

    /// Folds a sample into the interpolation.
    fn fold_sample(&mut self, _sample: Self::FillSample) {}

    /// Folds an aggregation into the interpolation.
    fn fold_aggregation(&mut self, _aggregation: A) {}
}

/// An interpolation over a constant sample.
#[derive(Debug)]
pub enum Constant {}

impl Constant {
    pub fn default<T>() -> ConstantState<T>
    where
        T: Default,
    {
        ConstantState::default()
    }

    pub fn new<T>(sample: T) -> ConstantState<T> {
        ConstantState(sample)
    }
}

impl Interpolation for Constant {
    type FillSample<F> = F::Sample
    where
        F: Statistic;
    type State<F> = ConstantState<F::Sample>
    where
        F: Statistic;
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ConstantState<T>(pub T);

impl<T, A> InterpolationState<A> for ConstantState<T>
where
    T: Clone,
{
    type FillSample = T;

    fn sample(&self) -> Self::FillSample {
        self.0.clone()
    }
}

/// An interpolation over the last observed sample.
#[derive(Debug)]
pub enum LastSample {}

impl LastSample {
    pub fn or<T>(sample: T) -> LastSampleState<T> {
        LastSampleState::or(sample)
    }
}

impl Interpolation for LastSample {
    type FillSample<F> = F::Sample
    where
        F: Statistic;
    type State<F> = LastSampleState<F::Sample>
    where
        F: Statistic;
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LastSampleState<T>(T);

impl<T> LastSampleState<T> {
    pub fn or(sample: T) -> Self {
        LastSampleState(sample)
    }
}

impl<T, A> InterpolationState<A> for LastSampleState<T>
where
    T: Clone,
{
    type FillSample = T;

    fn sample(&self) -> Self::FillSample {
        self.0.clone()
    }

    fn fold_sample(&mut self, sample: Self::FillSample) {
        let _prev = mem::replace(&mut self.0, sample);
    }
}

/// An interpolation over the last interval aggregation.
#[derive(Debug)]
pub enum LastAggregation {}

impl LastAggregation {
    pub fn or<A>(aggregation: A) -> LastAggregationState<A> {
        LastAggregationState::or(aggregation)
    }
}

impl Interpolation for LastAggregation {
    type FillSample<F> = F::Aggregation
    where
        F: Statistic;
    type State<F> = LastAggregationState<F::Aggregation>
    where
        F: Statistic;
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LastAggregationState<A>(A);

impl<A> LastAggregationState<A> {
    pub fn or(aggregation: A) -> Self {
        LastAggregationState(aggregation)
    }
}

impl<A> InterpolationState<A> for LastAggregationState<A>
where
    A: Clone,
{
    type FillSample = A;

    fn sample(&self) -> Self::FillSample {
        self.0.clone()
    }

    fn fold_aggregation(&mut self, aggregation: A) {
        let _prev = mem::replace(&mut self.0, aggregation);
    }
}

#[cfg(test)]
mod tests {
    use crate::experimental::series::interpolation::{
        Constant, InterpolationState, LastAggregation, LastSample,
    };

    /// Erases type information such that the `InterpolationState` returned by `f` only supports
    /// `u64` samples and aggregations.
    ///
    /// This avoids the need for verbose type annotations in tests.
    fn erase<P, F>(mut f: F) -> impl InterpolationState<u64, FillSample = u64>
    where
        F: FnMut() -> P,
        P: InterpolationState<u64, FillSample = u64>,
    {
        f()
    }

    #[test]
    fn interpolate_constant() {
        let mut interpolation = erase(|| Constant::new(1u64));
        assert_eq!(interpolation.sample(), 1);

        interpolation.fold_sample(7);
        interpolation.fold_aggregation(7);
        assert_eq!(interpolation.sample(), 1); // Both samples and aggregations are ignored.
    }

    #[test]
    fn interpolate_last_aggregation() {
        let mut interpolation = erase(|| LastAggregation::or(1u64));
        assert_eq!(interpolation.sample(), 1);

        interpolation.fold_sample(7);
        assert_eq!(interpolation.sample(), 1);

        interpolation.fold_aggregation(7); // Last aggregation is cached.
        assert_eq!(interpolation.sample(), 7);
    }

    #[test]
    fn interpolate_last_sample() {
        let mut interpolation = erase(|| LastSample::or(1u64));
        assert_eq!(interpolation.sample(), 1);

        interpolation.fold_aggregation(7);
        assert_eq!(interpolation.sample(), 1);

        interpolation.fold_sample(7); // Last sample is cached.
        assert_eq!(interpolation.sample(), 7);
    }
}
