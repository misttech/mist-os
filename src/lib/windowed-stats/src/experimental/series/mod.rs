// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Round-robin multi-resolution time series.

pub mod interpolation;
pub mod statistic;

use derivative::Derivative;
use num::{Num, Unsigned};
use std::convert::Infallible;
use std::fmt::Debug;
use std::io;
use std::marker::PhantomData;
use thiserror::Error;

use crate::experimental::clock::MonotonicityError;
use crate::experimental::series::statistic::OverflowError;

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

impl<T> DataSemantic for Gauge<T>
where
    T: Clone + Num,
{
    type Sample = T;
}
