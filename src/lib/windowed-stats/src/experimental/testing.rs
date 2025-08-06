// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use derivative::Derivative;
use fuchsia_sync::Mutex;
use std::any::{self, Any};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use crate::experimental::clock::{Timed, Timestamp};
use crate::experimental::series::buffer::BufferStrategy;
use crate::experimental::series::interpolation::Interpolation;
use crate::experimental::series::statistic::{Metadata, Statistic};
use crate::experimental::series::{
    FoldError, Interpolator, MatrixSampler, Sampler, SerializedBuffer, TimeMatrix,
};
use crate::experimental::serve::{
    BufferedSampler, InspectSender, InspectedTimeMatrix, ServedTimeMatrix,
};

type DynamicSample = Box<dyn Any + Send>;

#[derive(Derivative)]
#[derivative(Debug, PartialEq)]
pub enum TimeMatrixCall<T> {
    Fold(Timed<T>),
    Interpolate(Timestamp),
}

impl<T> TimeMatrixCall<T> {
    fn map<U, F>(self, f: F) -> TimeMatrixCall<U>
    where
        F: FnOnce(T) -> U,
    {
        match self {
            TimeMatrixCall::Fold(timed) => TimeMatrixCall::Fold(timed.map(f)),
            TimeMatrixCall::Interpolate(timestamp) => TimeMatrixCall::Interpolate(timestamp),
        }
    }
}

impl<T, E> TimeMatrixCall<Result<T, E>> {
    fn transpose(self) -> Result<TimeMatrixCall<T>, E> {
        match self {
            TimeMatrixCall::Fold(result) => match result.transpose() {
                Ok(sample) => Ok(TimeMatrixCall::Fold(sample)),
                Err(error) => Err(error),
            },
            TimeMatrixCall::Interpolate(timestamp) => Ok(TimeMatrixCall::Interpolate(timestamp)),
        }
    }
}

#[derive(Debug)]
pub struct TimeMatrixCallLog {
    calls: HashMap<String, Vec<TimeMatrixCall<DynamicSample>>>,
}

impl TimeMatrixCallLog {
    pub fn drain<T: Any + Send + Clone>(&mut self, name: &str) -> Vec<TimeMatrixCall<T>> {
        self.calls
            .remove(name)
            .unwrap_or_default()
            .into_iter()
            .map(|call| call.map(|sample| sample.downcast::<T>().map(|sample| *sample)))
            .map(TimeMatrixCall::transpose)
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|_| {
                panic!(
                    "in time matrix \"{}\": failed to downcast dynamic sample of type `{}`",
                    name,
                    any::type_name::<T>()
                )
            })
    }

    pub fn as_hash_map(&self) -> &HashMap<String, Vec<TimeMatrixCall<DynamicSample>>> {
        &self.calls
    }

    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }
}

#[derive(Clone)]
pub struct MockTimeMatrixClient {
    matrices: Arc<Mutex<Vec<Box<dyn ServedTimeMatrix>>>>,
    calls: Arc<Mutex<Vec<(String, TimeMatrixCall<DynamicSample>)>>>,
}

impl MockTimeMatrixClient {
    pub fn new() -> Self {
        Self { matrices: Arc::new(Mutex::new(vec![])), calls: Arc::new(Mutex::new(vec![])) }
    }
}

impl MockTimeMatrixClient {
    pub fn fold_buffered_samples(&self) -> TimeMatrixCallLog {
        for matrix in self.matrices.lock().iter_mut() {
            matrix.fold_buffered_samples().unwrap();
        }
        let mut calls = HashMap::<_, Vec<_>>::new();
        for (name, call) in self.calls.lock().drain(..) {
            calls.entry(name).or_default().push(call);
        }
        TimeMatrixCallLog { calls }
    }
}

impl InspectSender for MockTimeMatrixClient {
    fn inspect_time_matrix<F, P>(
        &self,
        name: impl Into<String>,
        _matrix: TimeMatrix<F, P>,
    ) -> InspectedTimeMatrix<F::Sample>
    where
        TimeMatrix<F, P>: 'static + MatrixSampler<F::Sample> + Send,
        Metadata<F>: 'static + Send + Sync,
        F: BufferStrategy<F::Aggregation, P> + Statistic,
        F::Sample: Send,
        P: Interpolation<FillSample<F> = F::Sample>,
    {
        let name = name.into();
        let (sender, matrix) = BufferedSampler::from_time_matrix(MockTimeMatrix::new(
            name.clone(),
            self.calls.clone(),
        ));
        self.matrices.lock().push(Box::new(matrix));
        InspectedTimeMatrix::new(name, sender)
    }

    fn inspect_time_matrix_with_metadata<F, P>(
        &self,
        name: impl Into<String>,
        matrix: TimeMatrix<F, P>,
        _metadata: impl Into<Metadata<F>>,
    ) -> InspectedTimeMatrix<F::Sample>
    where
        TimeMatrix<F, P>: 'static + MatrixSampler<F::Sample> + Send,
        Metadata<F>: 'static + Send + Sync,
        F: BufferStrategy<F::Aggregation, P> + Statistic,
        F::Sample: Send,
        P: Interpolation<FillSample<F> = F::Sample>,
    {
        self.inspect_time_matrix(name, matrix)
    }
}

struct MockTimeMatrix<T> {
    name: String,
    calls: Arc<Mutex<Vec<(String, TimeMatrixCall<DynamicSample>)>>>,
    phantom: PhantomData<fn() -> T>,
}

impl<T> MockTimeMatrix<T> {
    pub fn new(
        name: impl Into<String>,
        calls: Arc<Mutex<Vec<(String, TimeMatrixCall<DynamicSample>)>>>,
    ) -> Self {
        MockTimeMatrix { name: name.into(), calls, phantom: PhantomData }
    }
}

impl<T: Send + 'static> Sampler<Timed<T>> for MockTimeMatrix<T> {
    type Error = FoldError;

    fn fold(&mut self, sample: Timed<T>) -> Result<(), Self::Error> {
        let sample = sample.map(|v| Box::new(v) as DynamicSample);
        self.calls.lock().push((self.name.clone(), TimeMatrixCall::Fold(sample)));
        Ok(())
    }
}

impl<T> Interpolator for MockTimeMatrix<T> {
    type Error = FoldError;

    fn interpolate(&mut self, timestamp: Timestamp) -> Result<(), Self::Error> {
        self.calls.lock().push((self.name.clone(), TimeMatrixCall::Interpolate(timestamp)));
        Ok(())
    }
    fn interpolate_and_get_buffers(
        &mut self,
        timestamp: Timestamp,
    ) -> Result<SerializedBuffer, Self::Error> {
        self.interpolate(timestamp)?;
        Ok(SerializedBuffer { data_semantic: "mock".to_string(), data: vec![] })
    }
}

impl<T: Send + 'static> MatrixSampler<T> for MockTimeMatrix<T> {}
