// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::format_err;
use derivative::Derivative;
use fuchsia_inspect::{Inspector, Node as InspectNode};
use fuchsia_sync::Mutex;
use futures::channel::mpsc;
use futures::{select, Future, FutureExt, StreamExt};
use std::sync::Arc;
use tracing::{error, warn};
use {fuchsia_async as fasync, fuchsia_zircon as zx};

use crate::experimental::clock::{TimedSample, Timestamp};
use crate::experimental::series::{FoldError, Interpolator, RoundRobinSampler};

/// Capacity of "first come, first serve" slots available to clients of
/// the mpsc::Sender<TelemetryEvent>.
const TIME_MATRIX_SENDER_BUFFER_SIZE: usize = 10;

/// How often to interpolate time series stats.
const INTERPOLATE_INTERVAL: zx::Duration = zx::Duration::from_minutes(5);

/// Create a `TimeMatrixClient` and a `Future` server for routine management tasks of time
/// matrices.
/// The caller of this function is responsible for running the server.
pub fn serve_time_matrix_inspection(
    node: InspectNode,
) -> (TimeMatrixClient, impl Future<Output = Result<(), anyhow::Error>>) {
    let (sender, mut receiver) = mpsc::channel::<Arc<Mutex<dyn Interpolator<Error = FoldError>>>>(
        TIME_MATRIX_SENDER_BUFFER_SIZE,
    );
    let manager = TimeMatrixClient::new(sender, node.clone_weak());

    let fut = async move {
        let _node = node;
        let mut time_matrices = vec![];

        let mut interpolate_interval = fasync::Interval::new(INTERPOLATE_INTERVAL);
        loop {
            select! {
                time_matrix = receiver.next() => {
                    let Some(time_matrix) = time_matrix else {
                        error!("TimeMatrix stream unexpectedly terminated.");
                        return Err(format_err!("TimeMatrix stream unexpectedly terminated."));
                    };
                    time_matrices.push(time_matrix);
                }
                _ = interpolate_interval.next() => {
                    let now = Timestamp::now();
                    for time_matrix in &time_matrices {
                        if let Err(e) = time_matrix.lock().interpolate(now) {
                            warn!("Failed to interpolate {}: {:?}", "TODO: add name", e);
                        }
                    }
                }
            }
        }
    };
    (manager, fut)
}

type SharedTimeMatrix = Arc<Mutex<dyn Interpolator<Error = FoldError>>>;

pub struct TimeMatrixClient {
    sender: Arc<Mutex<mpsc::Sender<SharedTimeMatrix>>>,
    node: InspectNode,
}

impl Clone for TimeMatrixClient {
    fn clone(&self) -> Self {
        Self { sender: self.sender.clone(), node: self.node.clone_weak() }
    }
}

impl TimeMatrixClient {
    fn new(sender: mpsc::Sender<SharedTimeMatrix>, node: InspectNode) -> Self {
        Self { sender: Arc::new(Mutex::new(sender)), node: node }
    }

    /// Record TimeMatrix lazily into Inspect.
    /// Also send TimeMatrix to the server future for management.
    pub fn inspect_time_matrix<T>(
        &self,
        name: impl Into<String>,
        time_matrix: impl RoundRobinSampler<T> + Send + 'static,
    ) -> InspectedTimeMatrix<T> {
        let name = name.into();
        let time_matrix = Arc::new(Mutex::new(time_matrix));
        record_lazy_time_matrix(&self.node, &name, time_matrix.clone());
        if let Err(e) = self.sender.lock().try_send(time_matrix.clone()) {
            error!("Failed to process TimeMatrix {}: {:?}", name, e);
        }
        InspectedTimeMatrix::new(name, time_matrix)
    }
}

#[derive(Derivative)]
#[derivative(Debug)]
pub struct InspectedTimeMatrix<T> {
    name: String,
    #[derivative(Debug = "ignore")]
    time_matrix: Arc<Mutex<dyn RoundRobinSampler<T> + Send>>,
}

impl<T> InspectedTimeMatrix<T> {
    pub(crate) fn new(
        name: impl Into<String>,
        time_matrix: Arc<Mutex<dyn RoundRobinSampler<T> + Send>>,
    ) -> Self {
        Self { name: name.into(), time_matrix }
    }
}

impl<T> InspectedTimeMatrix<T> {
    pub fn fold(&self, sample: TimedSample<T>) -> Result<(), FoldError> {
        self.time_matrix.lock().fold(sample)
    }

    pub fn fold_or_log_error(&self, sample: TimedSample<T>) {
        if let Err(e) = self.time_matrix.lock().fold(sample) {
            warn!("Failed logging {} sample: {:?}", self.name, e);
        }
    }
}

fn record_lazy_time_matrix(
    inspect_node: &InspectNode,
    name: impl Into<String>,
    time_matrix: Arc<Mutex<dyn Interpolator<Error = FoldError> + Send>>,
) {
    let name = name.into();
    let name_copy = name.to_string();
    inspect_node.record_lazy_values(name, move || {
        let time_matrix = time_matrix.clone();
        let name = name_copy.clone();
        async move {
            let inspector = Inspector::default();
            {
                let now = Timestamp::now();
                let bytes = time_matrix.lock().interpolate_and_get_buffers(now);
                inspector
                    .root()
                    .record_bytes(name.clone(), bytes.unwrap_or(vec![0xba, 0xaa, 0xaa, 0xad]));
            }
            Ok(inspector)
        }
        .boxed()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuchsia_async as fasync;
    use futures::task::Poll;
    use std::pin::pin;

    use crate::experimental::series::{Interpolator, RoundRobinSampler, Sampler};

    #[test]
    fn test_serve_time_matrices() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let inspector = Inspector::default();
        let (manager, test_fut) =
            serve_time_matrix_inspection(inspector.root().create_child("time_series"));
        let mut test_fut = pin!(test_fut);

        let time_matrix = MockTimeMatrix::new();
        let _time_matrix_ref = manager.inspect_time_matrix("blah_blah", time_matrix.clone());

        let Poll::Pending = exec.run_until_stalled(&mut test_fut) else {
            panic!("test_fut has terminated");
        };
        assert_eq!(&time_matrix.lock().calls[..], &[]);

        exec.set_fake_time(fasync::Time::from_nanos(300_000_000_000));
        let Poll::Pending = exec.run_until_stalled(&mut test_fut) else {
            panic!("test_fut has terminated");
        };
        assert_eq!(
            &time_matrix.lock().calls[..],
            &[TimeMatrixCall::Interpolate(Timestamp::from_nanos(300_000_000_000)),]
        );
    }

    #[test]
    fn test_time_matrix_ref_fold() {
        let _exec = fasync::TestExecutor::new_with_fake_time();

        let inspector = Inspector::default();
        let (manager, _test_fut) =
            serve_time_matrix_inspection(inspector.root().create_child("time_series"));
        let time_matrix = MockTimeMatrix::new();
        let time_matrix_ref = manager.inspect_time_matrix("blah_blah", time_matrix.clone());
        assert!(time_matrix_ref.fold(TimedSample::now(1)).is_ok());
        assert_eq!(&time_matrix.lock().calls[..], &[TimeMatrixCall::Fold(TimedSample::now(1))]);
    }

    #[derive(Debug, PartialEq)]
    enum TimeMatrixCall {
        Fold(TimedSample<u64>),
        Interpolate(Timestamp),
    }

    struct MockTimeMatrix {
        calls: Vec<TimeMatrixCall>,
    }

    impl MockTimeMatrix {
        fn new() -> Arc<Mutex<Self>> {
            Arc::new(Mutex::new(Self { calls: vec![] }))
        }
    }

    impl Sampler<TimedSample<u64>> for Arc<Mutex<MockTimeMatrix>> {
        type Error = FoldError;
        fn fold(&mut self, sample: TimedSample<u64>) -> Result<(), Self::Error> {
            self.lock().calls.push(TimeMatrixCall::Fold(sample));
            Ok(())
        }
    }

    impl Interpolator for Arc<Mutex<MockTimeMatrix>> {
        type Error = FoldError;
        fn interpolate(&mut self, timestamp: Timestamp) -> Result<(), Self::Error> {
            self.lock().calls.push(TimeMatrixCall::Interpolate(timestamp));
            Ok(())
        }
        fn interpolate_and_get_buffers(
            &mut self,
            timestamp: Timestamp,
        ) -> Result<Vec<u8>, Self::Error> {
            self.interpolate(timestamp)?;
            Ok(vec![])
        }
    }

    impl RoundRobinSampler<u64> for Arc<Mutex<MockTimeMatrix>> {}
}
