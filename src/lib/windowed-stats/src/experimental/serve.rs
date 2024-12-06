// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use derivative::Derivative;
use fuchsia_async as fasync;
use fuchsia_inspect::{Inspector, Node as InspectNode};
use fuchsia_sync::Mutex;
use futures::channel::mpsc;
use futures::{select, Future, FutureExt, StreamExt};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::experimental::clock::{Timed, Timestamp};
use crate::experimental::series::buffer::BufferStrategy;
use crate::experimental::series::interpolation::Interpolation;
use crate::experimental::series::statistic::{Metadata, Statistic};
use crate::experimental::series::{FoldError, Interpolator, MatrixSampler, TimeMatrix};

// TODO(https://fxbug.dev/375489301): It is not possible to inject a mock time matrix into this
//                                    function. Refactor the function so that a unit test can
//                                    assert that interpolation occurs after an interval of time.
/// Creates a client and server for interpolating and recording time matrices to the given [Inspect
/// node][`Node`].
///
/// The client end can be used to instrument and send a [`TimeMatrix`] to the server. The server
/// end must be polled to incorporate and interpolate time matrices.
///
/// [`Node`]: fuchsia_inspect::Node
/// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
pub fn serve_time_matrix_inspection(
    node: InspectNode,
) -> (TimeMatrixClient, impl Future<Output = Result<(), anyhow::Error>>) {
    /// The buffer capacity of the MPSC channel through which time matrices are sent from clients
    /// to the server future.
    const TIME_MATRIX_SENDER_BUFFER_SIZE: usize = 10;

    /// The duration between interpolating data in inspected time matrices.
    const INTERPOLATION_PERIOD: zx::MonotonicDuration = zx::MonotonicDuration::from_minutes(5);

    let (sender, mut receiver) = mpsc::channel::<Arc<Mutex<dyn Interpolator<Error = FoldError>>>>(
        TIME_MATRIX_SENDER_BUFFER_SIZE,
    );

    let client = TimeMatrixClient::new(sender, node.clone_weak());
    let server = async move {
        let _node = node;
        let mut matrices = vec![];

        let mut interpolation = fasync::Interval::new(INTERPOLATION_PERIOD);
        loop {
            select! {
                // Incorporate any matrices received from the client.
                matrix = receiver.next() => {
                    match matrix {
                        Some(matrix) => matrices.push(matrix),
                        None => info!("time matrix inspection terminated."),
                    }
                }
                // Periodically interpolate data in each matrix.
                _ = interpolation.next() => {
                    let now = Timestamp::now();
                    for matrix in matrices.iter() {
                        if let Err(error) = matrix.lock().interpolate(now) {
                            // TODO(https://fxbug.dev/375255877): Log more information, such as the
                            //                                    name associated with the matrix.
                            warn!("failed to interpolate time matrix: {:?}", error);
                        }
                    }
                }
            }
        }
    };
    (client, server)
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
        Self { sender: Arc::new(Mutex::new(sender)), node }
    }

    /// Sends a [`TimeMatrix`] to the client's inspection server.
    ///
    /// See [`inspect_time_matrix_with_metadata`].
    ///
    /// [`inspect_time_matrix_with_metadata`]: crate::experimental::serve::TimeMatrixClient::inspect_time_matrix_with_metadata
    /// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
    pub fn inspect_time_matrix<F, P>(
        &self,
        name: impl Into<String>,
        matrix: TimeMatrix<F, P>,
    ) -> InspectedTimeMatrix<F::Sample>
    where
        TimeMatrix<F, P>: 'static + MatrixSampler<F::Sample> + Send,
        Metadata<F>: 'static + Send + Sync,
        F: BufferStrategy<F::Aggregation, P> + Statistic,
        P: Interpolation<FillSample<F> = F::Sample>,
    {
        self.inspect_and_record_with(name, matrix, |_node| {})
    }

    /// Sends a [`TimeMatrix`] to the client's inspection server.
    ///
    /// This function lazily records the given [`TimeMatrix`] to Inspect. The server end
    /// periodically interpolates the matrix and records data as needed. The returned
    /// [handle][`InspectedTimeMatrix`] can be used to fold samples into the matrix.
    ///
    /// [`InspectedTimeMatrix`]: crate::experimental::serve::InspectedTimeMatrix
    /// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
    pub fn inspect_time_matrix_with_metadata<F, P>(
        &self,
        name: impl Into<String>,
        matrix: TimeMatrix<F, P>,
        metadata: impl Into<Metadata<F>>,
    ) -> InspectedTimeMatrix<F::Sample>
    where
        TimeMatrix<F, P>: 'static + MatrixSampler<F::Sample> + Send,
        Metadata<F>: 'static + Send + Sync,
        F: BufferStrategy<F::Aggregation, P> + Statistic,
        P: Interpolation<FillSample<F> = F::Sample>,
    {
        let metadata = Arc::new(metadata.into());
        self.inspect_and_record_with(name, matrix, move |node| {
            use crate::experimental::series::metadata::Metadata;

            node.record_child("metadata", |node| {
                metadata.record(node);
            })
        })
    }

    fn inspect_and_record_with<F, P, R>(
        &self,
        name: impl Into<String>,
        matrix: TimeMatrix<F, P>,
        record: R,
    ) -> InspectedTimeMatrix<F::Sample>
    where
        TimeMatrix<F, P>: 'static + MatrixSampler<F::Sample> + Send,
        Metadata<F>: 'static + Send + Sync,
        F: BufferStrategy<F::Aggregation, P> + Statistic,
        P: Interpolation<FillSample<F> = F::Sample>,
        R: 'static + Clone + Fn(&InspectNode) + Send + Sync,
    {
        let name = name.into();
        let matrix = Arc::new(Mutex::new(matrix));
        self::record_lazy_time_matrix_with(&self.node, &name, matrix.clone(), record);
        if let Err(error) = self.sender.lock().try_send(matrix.clone()) {
            error!("failed to send time matrix \"{}\" to inspection server: {:?}", name, error);
        }
        InspectedTimeMatrix::new(name, matrix)
    }
}

#[derive(Derivative)]
#[derivative(Debug, Clone)]
pub struct InspectedTimeMatrix<T> {
    name: String,
    #[derivative(Debug = "ignore")]
    matrix: Arc<Mutex<dyn MatrixSampler<T> + Send>>,
}

impl<T> InspectedTimeMatrix<T> {
    pub(crate) fn new(
        name: impl Into<String>,
        matrix: Arc<Mutex<dyn MatrixSampler<T> + Send>>,
    ) -> Self {
        Self { name: name.into(), matrix }
    }

    pub fn fold(&self, sample: Timed<T>) -> Result<(), FoldError> {
        self.matrix.lock().fold(sample)
    }

    pub fn fold_or_log_error(&self, sample: Timed<T>) {
        if let Err(error) = self.matrix.lock().fold(sample) {
            warn!("failed to fold sample into time matrix \"{}\": {:?}", self.name, error);
        }
    }
}

/// Records a lazy child node in the given node that records buffers and metadata for the given
/// time matrix.
///
/// The function `f` is passed a node to record arbitrary data after the data semantic and buffers
/// of the time matrix have been recorded using that same node.
fn record_lazy_time_matrix_with<F>(
    node: &InspectNode,
    name: impl Into<String>,
    matrix: Arc<Mutex<dyn Interpolator<Error = FoldError> + Send>>,
    f: F,
) where
    F: 'static + Clone + Fn(&InspectNode) + Send + Sync,
{
    let name = name.into();
    node.record_lazy_child(name, move || {
        let matrix = matrix.clone();
        let f = f.clone();
        async move {
            let inspector = Inspector::default();
            {
                let now = Timestamp::now();
                match matrix.lock().interpolate_and_get_buffers(now) {
                    Ok(buffer) => {
                        inspector.root().atomic_update(|node| {
                            node.record_string("type", buffer.data_semantic);
                            node.record_bytes("data", buffer.data);
                            f(node);
                        });
                    }
                    Err(error) => {
                        inspector.root().record_string("type", format!("error: {:?}", error));
                    }
                }
            }
            Ok(inspector)
        }
        .boxed()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, AnyBytesProperty};
    use fuchsia_async as fasync;
    use futures::task::Poll;
    use std::mem;
    use std::pin::pin;

    use crate::experimental::series::interpolation::LastSample;
    use crate::experimental::series::metadata::BitSetMap;
    use crate::experimental::series::statistic::Union;

    #[test]
    fn serve_time_matrix_inspection_then_inspect_data_tree_contains_buffers() {
        let _executor = fasync::TestExecutor::new_with_fake_time();

        let inspector = Inspector::default();
        let (client, _server) =
            serve_time_matrix_inspection(inspector.root().create_child("serve_test_node"));
        let _matrix = client
            .inspect_time_matrix("connectivity", TimeMatrix::<Union<u64>, LastSample>::default());

        assert_data_tree!(inspector, root: contains {
            serve_test_node: {
                connectivity: {
                    "type": "bitset",
                    "data": AnyBytesProperty,
                }
            }
        });
    }

    #[test]
    fn serve_time_matrix_inspection_with_metadata_then_inspect_data_tree_contains_metadata() {
        let _executor = fasync::TestExecutor::new_with_fake_time();

        let inspector = Inspector::default();
        let (client, _server) =
            self::serve_time_matrix_inspection(inspector.root().create_child("serve_test_node"));
        let _matrix = client.inspect_time_matrix_with_metadata(
            "engine",
            TimeMatrix::<Union<u64>, LastSample>::default(),
            BitSetMap::from_ordered(["check", "oil", "battery", "coolant"]),
        );

        assert_data_tree!(inspector, root: contains {
            serve_test_node: {
                engine: {
                    "type": "bitset",
                    "data": AnyBytesProperty,
                    metadata: {
                        index: {
                            "0": "check",
                            "1": "oil",
                            "2": "battery",
                            "3": "coolant",
                        }
                    }
                }
            }
        });
    }

    #[test]
    fn drop_time_matrix_client_then_server_continues_execution() {
        let mut executor = fasync::TestExecutor::new_with_fake_time();

        let inspector = Inspector::default();
        let (client, server) =
            serve_time_matrix_inspection(inspector.root().create_child("serve_test_node"));
        let mut server = pin!(server);

        mem::drop(client);

        // The server future should continue execution even if its associated client is dropped.
        let Poll::Pending = executor.run_until_stalled(&mut server) else {
            panic!("time matrix inspection server terminated unexpectedly");
        };
    }
}
