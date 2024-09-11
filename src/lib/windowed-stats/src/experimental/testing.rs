// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use derivative::Derivative;
use fuchsia_sync::Mutex;
use std::sync::Arc;

use crate::experimental::clock::{TimedSample, Timestamp};
use crate::experimental::series::{
    FoldError, Interpolator, RoundRobinSampler, Sampler, SerializedBuffer,
};
use crate::experimental::serve::InspectedTimeMatrix;

#[derive(Derivative)]
#[derivative(Debug, PartialEq)]
pub enum TimeMatrixCall<T> {
    Fold(TimedSample<T>),
    Interpolate(Timestamp),
}

#[derive(Derivative)]
#[derivative(Debug, Clone(bound = ""), Default)]
pub struct MockTimeMatrix<T> {
    calls: Arc<Mutex<Vec<TimeMatrixCall<T>>>>,
}

impl<T> MockTimeMatrix<T> {
    pub fn drain_calls(&self) -> Vec<TimeMatrixCall<T>> {
        self.calls.lock().drain(..).collect()
    }
}

impl<T: Send + 'static> MockTimeMatrix<T> {
    pub fn build_ref(&self, name: &str) -> InspectedTimeMatrix<T> {
        InspectedTimeMatrix::new(name, Arc::new(Mutex::new((*self).clone())))
    }
}

impl<T> Sampler<TimedSample<T>> for MockTimeMatrix<T> {
    type Error = FoldError;
    fn fold(&mut self, sample: TimedSample<T>) -> Result<(), Self::Error> {
        self.calls.lock().push(TimeMatrixCall::Fold(sample));
        Ok(())
    }
}

impl<T> Interpolator for MockTimeMatrix<T> {
    type Error = FoldError;
    fn interpolate(&mut self, timestamp: Timestamp) -> Result<(), Self::Error> {
        self.calls.lock().push(TimeMatrixCall::Interpolate(timestamp));
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

impl<T> RoundRobinSampler<T> for MockTimeMatrix<T> {}
