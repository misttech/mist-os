// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect::{Inspector, Node as InspectNode};
use fuchsia_sync::Mutex as SyncMutex;
use futures::channel::mpsc;
use futures::lock::Mutex as AsyncMutex;
use futures::{select, Future, FutureExt as _, StreamExt as _};
use log::{error, info, warn};
use std::sync::Arc;
use {async_channel as mpmc, fuchsia_async as fasync};

use crate::experimental::clock::{Timed, Timestamp};
use crate::experimental::series::buffer::BufferStrategy;
use crate::experimental::series::interpolation::Interpolation;
use crate::experimental::series::statistic::{Metadata, Statistic};
use crate::experimental::series::{
    FoldError, Interpolator, MatrixSampler, SerializedBuffer, TimeMatrix,
};
use crate::experimental::vec1::Vec1;

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
    const TIME_MATRIX_SENDER_BUFFER_SIZE: usize = 250;

    /// The duration between interpolating data in inspected time matrices.
    const INTERPOLATION_PERIOD: zx::MonotonicDuration = zx::MonotonicDuration::from_minutes(5);

    let (sender, mut receiver) = mpsc::channel::<SharedTimeMatrix>(TIME_MATRIX_SENDER_BUFFER_SIZE);

    let client = TimeMatrixClient::new(sender, node.clone_weak());
    let server = async move {
        let _node = node;
        let mut matrices = vec![];

        let mut interpolation = fasync::Interval::new(INTERPOLATION_PERIOD);
        loop {
            select! {
                // Incorporate time matrices received from the client.
                matrix = receiver.next() => {
                    match matrix {
                        Some(matrix) => {
                            matrices.push(matrix);
                        }
                        None => {
                            info!("time matrix inspection terminated.");
                        }
                    }
                }
                // Periodically fold buffered samples into and interpolate time matrices.
                _ = interpolation.next() => {
                    // TODO(https://fxbug.dev/375255877): Log more information, such as the name
                    //                                    associated with the matrix.
                    for matrix in matrices.iter() {
                        let mut matrix = matrix.lock().await;
                        if let Err(error) = matrix.fold_buffered_samples() {
                            warn!("failed to fold samples into time matrix: {:?}", error);
                        }
                        // Querying the current timestamp for each matrix like this introduces a
                        // bias: the more recently a matrix has been pushed into `matrices`, the
                        // more recent the timestamp of its interpolation. However, folding
                        // buffered samples may take a non-trivial amount of time and a sample may
                        // arrive as a buffer is being drained. The current timestamp must be
                        // queried after the drain is complete to guarantee that it is more recent
                        // than any timestamp associated with a sample.
                        if let Err(error) = matrix.interpolate(Timestamp::now()) {
                            warn!("failed to interpolate time matrix: {:?}", error);
                        }
                    }
                }
            }
        }
    };
    (client, server)
}

pub trait ServedTimeMatrix: Interpolator<Error = FoldError> + Send {
    fn fold_buffered_samples(&mut self) -> Result<(), Self::Error>;
}

pub struct BufferedSampler<T, M>
where
    M: MatrixSampler<T>,
{
    receiver: mpmc::Receiver<Timed<T>>,
    matrix: M,
}

impl<T, M> BufferedSampler<T, M>
where
    M: MatrixSampler<T>,
{
    pub fn from_time_matrix(matrix: M) -> (mpmc::Sender<Timed<T>>, Self) {
        /// The buffer capacity of the MPMC channel through which timed samples are sent to
        /// `BufferedSampler`s.
        const TIMED_SAMPLE_SENDER_BUFFER_SIZE: usize = 1024;

        let (sender, receiver) = mpmc::bounded(TIMED_SAMPLE_SENDER_BUFFER_SIZE);
        (sender, BufferedSampler { receiver, matrix })
    }
}

impl<T, M> Interpolator for BufferedSampler<T, M>
where
    M: MatrixSampler<T>,
{
    type Error = FoldError;

    fn interpolate(&mut self, timestamp: Timestamp) -> Result<(), Self::Error> {
        self.matrix.interpolate(timestamp)
    }

    fn interpolate_and_get_buffers(
        &mut self,
        timestamp: Timestamp,
    ) -> Result<SerializedBuffer, Self::Error> {
        self.matrix.interpolate_and_get_buffers(timestamp)
    }
}

impl<T, M> ServedTimeMatrix for BufferedSampler<T, M>
where
    T: Send,
    M: MatrixSampler<T> + Send,
{
    fn fold_buffered_samples(&mut self) -> Result<(), Self::Error> {
        let mut errors = vec![];
        loop {
            match self.receiver.try_recv() {
                Ok(sample) => {
                    if let Err(error) = self.matrix.fold(sample) {
                        errors.push(error);
                    }
                }
                Err(error) => {
                    return match error {
                        mpmc::TryRecvError::Closed => Err(FoldError::Buffer),
                        mpmc::TryRecvError::Empty => match Vec1::try_from(errors) {
                            Ok(errors) => Err(FoldError::Flush(errors)),
                            _ => Ok(()),
                        },
                    };
                }
            }
        }
    }
}

pub trait InspectSender {
    /// Sends a [`TimeMatrix`] to the client's inspection server.
    ///
    /// See [`inspect_time_matrix_with_metadata`].
    ///
    /// [`inspect_time_matrix_with_metadata`]: crate::experimental::serve::TimeMatrixClient::inspect_time_matrix_with_metadata
    /// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
    fn inspect_time_matrix<F, P>(
        &self,
        name: impl Into<String>,
        matrix: TimeMatrix<F, P>,
    ) -> InspectedTimeMatrix<F::Sample>
    where
        TimeMatrix<F, P>: 'static + MatrixSampler<F::Sample> + Send,
        Metadata<F>: 'static + Send + Sync,
        F: BufferStrategy<F::Aggregation, P> + Statistic,
        F::Sample: Send,
        P: Interpolation<FillSample<F> = F::Sample>;

    /// Sends a [`TimeMatrix`] to the client's inspection server.
    ///
    /// This function lazily records the given [`TimeMatrix`] to Inspect. The server end
    /// periodically interpolates the matrix and records data as needed. The returned
    /// [handle][`InspectedTimeMatrix`] can be used to fold samples into the matrix.
    ///
    /// [`InspectedTimeMatrix`]: crate::experimental::serve::InspectedTimeMatrix
    /// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
    fn inspect_time_matrix_with_metadata<F, P>(
        &self,
        name: impl Into<String>,
        matrix: TimeMatrix<F, P>,
        metadata: impl Into<Metadata<F>>,
    ) -> InspectedTimeMatrix<F::Sample>
    where
        TimeMatrix<F, P>: 'static + MatrixSampler<F::Sample> + Send,
        Metadata<F>: 'static + Send + Sync,
        F: BufferStrategy<F::Aggregation, P> + Statistic,
        F::Sample: Send,
        P: Interpolation<FillSample<F> = F::Sample>;
}

type SharedTimeMatrix = Arc<AsyncMutex<dyn ServedTimeMatrix>>;

pub struct TimeMatrixClient {
    // TODO(https://fxbug.dev/432324973): Synchronizing the sender end of a channel like this is an
    //                                    anti-pattern. Consider removing the mutex. See the linked
    //                                    bug for discussion of the ramifications.
    sender: Arc<SyncMutex<mpsc::Sender<SharedTimeMatrix>>>,
    node: InspectNode,
}

impl TimeMatrixClient {
    fn new(sender: mpsc::Sender<SharedTimeMatrix>, node: InspectNode) -> Self {
        Self { sender: Arc::new(SyncMutex::new(sender)), node }
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
        F::Sample: Send,
        P: Interpolation<FillSample<F> = F::Sample>,
        R: 'static + Clone + Fn(&InspectNode) + Send + Sync,
    {
        let name = name.into();
        let (sender, matrix) = BufferedSampler::from_time_matrix(matrix);
        let matrix = Arc::new(AsyncMutex::new(matrix));
        self::record_lazy_time_matrix_with(&self.node, &name, matrix.clone(), record);
        if let Err(error) = self.sender.lock().try_send(matrix) {
            error!("failed to send time matrix \"{}\" to inspection server: {:?}", name, error);
        }
        InspectedTimeMatrix::new(name, sender)
    }
}

impl Clone for TimeMatrixClient {
    fn clone(&self) -> Self {
        TimeMatrixClient { sender: self.sender.clone(), node: self.node.clone_weak() }
    }
}

impl InspectSender for TimeMatrixClient {
    fn inspect_time_matrix<F, P>(
        &self,
        name: impl Into<String>,
        matrix: TimeMatrix<F, P>,
    ) -> InspectedTimeMatrix<F::Sample>
    where
        TimeMatrix<F, P>: 'static + MatrixSampler<F::Sample> + Send,
        Metadata<F>: 'static + Send + Sync,
        F: BufferStrategy<F::Aggregation, P> + Statistic,
        F::Sample: Send,
        P: Interpolation<FillSample<F> = F::Sample>,
    {
        self.inspect_and_record_with(name, matrix, |_node| {})
    }

    fn inspect_time_matrix_with_metadata<F, P>(
        &self,
        name: impl Into<String>,
        matrix: TimeMatrix<F, P>,
        metadata: impl Into<Metadata<F>>,
    ) -> InspectedTimeMatrix<F::Sample>
    where
        TimeMatrix<F, P>: 'static + MatrixSampler<F::Sample> + Send,
        Metadata<F>: 'static + Send + Sync,
        F: BufferStrategy<F::Aggregation, P> + Statistic,
        F::Sample: Send,
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
}

#[derive(Debug, Clone)]
pub struct InspectedTimeMatrix<T> {
    name: String,
    sender: mpmc::Sender<Timed<T>>,
}

impl<T> InspectedTimeMatrix<T> {
    pub(crate) fn new(name: impl Into<String>, sender: mpmc::Sender<Timed<T>>) -> Self {
        Self { name: name.into(), sender }
    }

    pub fn fold(&self, sample: Timed<T>) -> Result<(), FoldError> {
        // TODO(https://fxbug.dev/432323121): Place the data that could not be sent into the
        //                                    channel into the error. See `FoldError`.
        self.sender.try_send(sample).map_err(|_| FoldError::Buffer)
    }

    pub fn fold_or_log_error(&self, sample: Timed<T>) {
        if let Err(error) = self.sender.try_send(sample) {
            warn!("failed to buffer sample for time matrix \"{}\": {:?}", self.name, error);
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
    matrix: Arc<AsyncMutex<dyn ServedTimeMatrix + Send>>,
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
                let mut matrix = matrix.lock().await;
                if let Err(error) = matrix
                    .fold_buffered_samples()
                    .and_then(|_| matrix.interpolate_and_get_buffers(Timestamp::now()))
                    .map(|buffer| {
                        inspector.root().atomic_update(|node| {
                            node.record_string("type", buffer.data_semantic);
                            node.record_bytes("data", buffer.data);
                            f(node);
                        })
                    })
                {
                    inspector.root().record_string("type", format!("error: {:?}", error));
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

    #[fuchsia::test]
    async fn serve_time_matrix_inspection_then_inspect_data_tree_contains_buffers() {
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

    #[fuchsia::test]
    async fn serve_time_matrix_inspection_with_metadata_then_inspect_data_tree_contains_metadata() {
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
