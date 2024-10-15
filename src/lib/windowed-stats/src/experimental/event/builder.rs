// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::experimental::clock::Timed;
use crate::experimental::event::reactor::Reactor;
use crate::experimental::event::Event;
use crate::experimental::series::buffer::BufferStrategy;
use crate::experimental::series::interpolation::Interpolation;
use crate::experimental::series::statistic::{Metadata, Statistic};
use crate::experimental::series::{FoldError, MatrixSampler, SamplingProfile, TimeMatrix};
use crate::experimental::serve::TimeMatrixClient;

/// Represents an optional builder field that has not been set.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unset;

/// Builds a [`Reactor`] that samples a [data record][`DataEvent::record`] with a [`TimeMatrix`].
///
/// See the [`event::sample_data_record`] function.
///
/// [`DataEvent::record`]: crate::experimental::event::DataEvent::record
/// [`event::sample_data_record`]: crate::experimental::event::sample_data_record
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
#[derive(Clone, Copy, Debug)]
pub struct SampleDataRecord<F, M = Unset> {
    statistic: F,
    metadata: M,
}

impl<F, M> SampleDataRecord<F, M>
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
        F: BufferStrategy<F::Aggregation, P>,
        P: Interpolation<FillSample<F> = F::Sample>,
    {
        let SampleDataRecord { statistic, .. } = self;
        let matrix = client.inspect_time_matrix(
            name.as_ref(),
            TimeMatrix::with_statistic(profile, interpolation, statistic),
        );
        move |event: Timed<Event<F::Sample>>| {
            if let Some(sample) = event.to_timed_sample() {
                matrix.fold(sample)
            } else {
                Ok(())
            }
        }
    }
}

impl<F> SampleDataRecord<F, Unset>
where
    F: Statistic,
{
    pub fn with_metadata(self, metadata: Metadata<F>) -> SampleDataRecord<F, Metadata<F>> {
        let SampleDataRecord { statistic, .. } = self;
        SampleDataRecord { statistic, metadata }
    }
}

/// Constructs a builder for a [`Reactor`] that samples a [data record][`DataEvent::record`] with a
/// [`TimeMatrix`].
///
/// [`DataEvent::record`]: crate::experimental::event::DataEvent::record
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
pub fn sample_data_record<F>(statistic: F) -> SampleDataRecord<F, Unset>
where
    F: Statistic,
{
    SampleDataRecord { statistic, metadata: Unset }
}
