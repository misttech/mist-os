// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::marker::PhantomData;

use crate::experimental::clock::Timed;
use crate::experimental::event::reactor::Reactor;
use crate::experimental::event::Event;
use crate::experimental::series::buffer::BufferStrategy;
use crate::experimental::series::interpolation::Interpolation;
use crate::experimental::series::statistic::{Metadata, Statistic};
use crate::experimental::series::{FoldError, MatrixSampler, SamplingProfile, TimeMatrix};
use crate::experimental::serve::{InspectedTimeMatrix, TimeMatrixClient};

/// A type that maps the presence of an optional builder field to another type.
pub trait Optional {
    type Field;
}

/// An optional builder field that has been set to a value of type `T`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Set<T>(PhantomData<fn() -> T>);

impl<T> Optional for Set<T> {
    type Field = T;
}

/// An optional builder field that has **not** been set.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unset;

impl Optional for Unset {
    type Field = ();
}

/// Builds a [`Reactor`] that samples a [data record][`DataEvent::record`] with a [`TimeMatrix`].
///
/// The [`TimeMatrix`] is send to [an Inspect server][`serve::serve_time_matrix_inspection] via a
/// given client.
///
/// See the [`event::sample_data_record`] function.
///
/// [`DataEvent::record`]: crate::experimental::event::DataEvent::record
/// [`event::sample_data_record`]: crate::experimental::event::sample_data_record
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`serve::serve_time_matrix_inspection`]: crate::experimental::serve::serve_time_matrix_inspection
/// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
#[derive(Clone, Copy, Debug)]
pub struct SampleDataRecord<F, M = Unset>
where
    M: Optional,
{
    statistic: F,
    metadata: M::Field,
}

impl<F, M> SampleDataRecord<F, M>
where
    M: Optional,
{
    fn reactor<T>(
        matrix: InspectedTimeMatrix<T>,
    ) -> impl Reactor<T, Response = (), Error = FoldError>
    where
        T: Clone,
    {
        move |event: Timed<Event<T>>| {
            if let Some(sample) = event.to_timed_sample() {
                matrix.fold(sample)
            } else {
                Ok(())
            }
        }
    }
}

impl<F> SampleDataRecord<F, Set<Metadata<F>>>
where
    F: Statistic,
{
    pub fn in_time_matrix<P>(
        self,
        client: &TimeMatrixClient,
        name: impl AsRef<str>,
        profile: SamplingProfile,
        interpolation: P::State<F>,
    ) -> impl Reactor<F::Sample, Response = (), Error = FoldError>
    where
        TimeMatrix<F, P>: 'static + MatrixSampler<F::Sample> + Send,
        Metadata<F>: 'static + Send + Sync,
        F: BufferStrategy<F::Aggregation, P> + Statistic,
        P: Interpolation<FillSample<F> = F::Sample>,
    {
        let SampleDataRecord { statistic, metadata } = self;
        let matrix = client.inspect_time_matrix_with_metadata(
            name.as_ref(),
            TimeMatrix::with_statistic(profile, interpolation, statistic),
            metadata,
        );
        Self::reactor(matrix)
    }
}

impl<F> SampleDataRecord<F, Unset>
where
    F: Statistic,
{
    /// Builds the [`Reactor`] with the given metadata for the [`TimeMatrix`].
    ///
    /// The type of `metadata` is determined by the [`DataSemantic`] of the [`Statistic`]. For
    /// example, the [`Union`] statistic has [`BitSet`] semantics and so requires types convertible
    /// into the [`BitSetIndex`] metadata type.
    ///
    /// [`BitSet`]: crate::experimental::series::BitSet
    /// [`BitSetIndex`]: crate::experimental::series::metadata::BitSetIndex
    /// [`DataSemantic`]: crate::experimental::series::DataSemantic
    /// [`Reactor`]: crate::experimental::event::Reactor
    /// [`Statistic`]: crate::experimental::series::statistic::Statistic
    /// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
    /// [`Union`]: crate::experimental::series::statistic::Union
    pub fn with_metadata(
        self,
        metadata: impl Into<Metadata<F>>,
    ) -> SampleDataRecord<F, Set<Metadata<F>>> {
        let SampleDataRecord { statistic, .. } = self;
        SampleDataRecord { statistic, metadata: metadata.into() }
    }

    pub fn in_time_matrix<P>(
        self,
        client: &TimeMatrixClient,
        name: impl AsRef<str>,
        profile: SamplingProfile,
        interpolation: P::State<F>,
    ) -> impl Reactor<F::Sample, Response = (), Error = FoldError>
    where
        TimeMatrix<F, P>: 'static + MatrixSampler<F::Sample> + Send,
        Metadata<F>: 'static + Send + Sync,
        F: BufferStrategy<F::Aggregation, P> + Statistic,
        P: Interpolation<FillSample<F> = F::Sample>,
    {
        let SampleDataRecord { statistic, .. } = self;
        let matrix = client.inspect_time_matrix(
            name.as_ref(),
            TimeMatrix::with_statistic(profile, interpolation, statistic),
        );
        Self::reactor(matrix)
    }
}

/// Constructs a builder for a [`Reactor`] that samples a [data record][`DataEvent::record`] with a
/// [`TimeMatrix`] using the given [`Statistic`].
///
/// [`DataEvent::record`]: crate::experimental::event::DataEvent::record
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`Statistic`]: crate::experimental::series::statistic::Statistic
/// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
pub fn sample_data_record<F>(statistic: F) -> SampleDataRecord<F, Unset>
where
    F: Statistic,
{
    SampleDataRecord { statistic, metadata: () }
}
