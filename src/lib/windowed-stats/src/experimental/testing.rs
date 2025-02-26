// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use derivative::Derivative;
use fuchsia_sync::Mutex;
use std::any::{type_name, Any};
use std::collections::HashMap;
use std::sync::Arc;

use crate::experimental::clock::{Timed, Timestamp};
use crate::experimental::series::buffer::BufferStrategy;
use crate::experimental::series::interpolation::Interpolation;
use crate::experimental::series::statistic::{Metadata, Statistic};
use crate::experimental::series::{
    FoldError, Interpolator, MatrixSampler, Sampler, SerializedBuffer, TimeMatrix,
};
use crate::experimental::serve::{InspectSender, InspectedTimeMatrix};

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

#[derive(Debug)]
pub struct TimeMatrixCalls {
    calls: HashMap<String, Vec<TimeMatrixCall<DynamicSample>>>,
}

impl TimeMatrixCalls {
    pub fn drain<T: Any + Send + Clone>(&mut self, name: &str) -> Vec<TimeMatrixCall<T>> {
        let mut calls = vec![];
        if let Some(inner_calls) = self.calls.remove(name) {
            for c in inner_calls {
                let call = c.map(|value| match value.downcast::<T>() {
                    Ok(v) => *v,
                    Err(_e) => panic!("Cannot convert captured call to type {}", type_name::<T>()),
                });
                calls.push(call);
            }
        }
        calls
    }
}

#[derive(Clone)]
pub struct MockTimeMatrixClient {
    calls: Arc<Mutex<Vec<(String, TimeMatrixCall<DynamicSample>)>>>,
}

impl MockTimeMatrixClient {
    pub fn new() -> Self {
        Self { calls: Arc::new(Mutex::new(vec![])) }
    }
}

impl MockTimeMatrixClient {
    pub fn drain_calls(&self) -> TimeMatrixCalls {
        let mut calls: HashMap<String, Vec<TimeMatrixCall<DynamicSample>>> = HashMap::new();
        for call in self.calls.lock().drain(..) {
            calls.entry(call.0).or_default().push(call.1);
        }
        TimeMatrixCalls { calls }
    }

    pub(crate) fn new_inspected_time_matrix<T: Send + 'static>(
        &self,
        name: impl Into<String>,
    ) -> InspectedTimeMatrix<T> {
        let name = name.into();
        let matrix = MockTimeMatrix {
            name: name.clone(),
            calls: Arc::clone(&self.calls),
            phantom: std::marker::PhantomData,
        };
        InspectedTimeMatrix::new(name, Arc::new(Mutex::new(matrix)))
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
        let matrix = MockTimeMatrix {
            name: name.clone(),
            calls: Arc::clone(&self.calls),
            phantom: std::marker::PhantomData,
        };
        InspectedTimeMatrix::new(name, Arc::new(Mutex::new(matrix)))
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
    phantom: std::marker::PhantomData<fn() -> T>,
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
