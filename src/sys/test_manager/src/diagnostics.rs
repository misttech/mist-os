// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::utilities::LogOnDrop,
    anyhow::Error,
    async_trait::async_trait,
    diagnostics_bridge::ArchiveReaderManager,
    diagnostics_data::{Data, LogsData},
    diagnostics_reader as reader,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_developer_remotecontrol::StreamError,
    fidl_fuchsia_diagnostics as fdiagnostics,
    fidl_fuchsia_diagnostics::{
        BatchIteratorMarker, ClientSelectorConfiguration, DataType, Format, StreamMode,
        StreamParameters,
    },
    fidl_fuchsia_diagnostics_host as fhost, fidl_fuchsia_test_manager as ftest_manager,
    fuchsia_async as fasync,
    futures::stream::FusedStream,
    tracing::warn,
};

pub(crate) struct ServeSyslogOutcome {
    /// Task serving any protocols needed to proxy logs. For example, this is populated
    /// when logs are served over overnet using DiagnosticsBridge.
    pub logs_iterator_task: Option<fasync::Task<Result<(), Error>>>,
}

/// Connect to archivist and starting serving syslog.
/// TODO(https://fxbug.dev/133153): Only take one ArchiveAccessorProxy, not both.
pub(crate) fn serve_syslog(
    accessor: fdiagnostics::ArchiveAccessorProxy,
    host_accessor: fhost::ArchiveAccessorProxy,
    log_iterator: ftest_manager::LogsIterator,
) -> Result<ServeSyslogOutcome, StreamError> {
    let logs_iterator_task = match log_iterator {
        ftest_manager::LogsIterator::Archive(iterator) => {
            let iterator_fut =
                IsolatedLogsProvider::new(&accessor).run_iterator_server(iterator)?;
            Some(fasync::Task::spawn(async move {
                let _on_drop = LogOnDrop("Log iterator task dropped");
                iterator_fut.await
            }))
        }
        ftest_manager::LogsIterator::Stream(iterator) => {
            let iterator_fut = run_iterator_socket(&host_accessor, iterator);
            Some(fasync::Task::spawn(async move {
                iterator_fut.await?;
                Ok(())
            }))
        }
        ftest_manager::LogsIterator::Batch(iterator) => {
            IsolatedLogsProvider::new(&accessor).start_streaming_logs(iterator)?;
            None
        }
        _ => None,
    };
    Ok(ServeSyslogOutcome { logs_iterator_task })
}

fn run_iterator_socket(
    host_accessor: &fhost::ArchiveAccessorProxy,
    socket: fuchsia_zircon::Socket,
) -> fidl::client::QueryResponseFut<()> {
    host_accessor.stream_diagnostics(
        &StreamParameters {
            stream_mode: Some(StreamMode::SnapshotThenSubscribe),
            data_type: Some(DataType::Logs),
            format: Some(Format::Json),
            client_selector_configuration: Some(ClientSelectorConfiguration::SelectAll(true)),
            batch_retrieval_timeout_seconds: None,
            ..Default::default()
        },
        socket,
    )
}

/// Type alias for &'a ArchiveAccessorProxy
/// so we can impl ArchiveReaderManager on it.
struct IsolatedLogsProvider<'a> {
    accessor: &'a fdiagnostics::ArchiveAccessorProxy,
}

impl<'a> IsolatedLogsProvider<'a> {
    fn new(accessor: &'a fdiagnostics::ArchiveAccessorProxy) -> Self {
        Self { accessor }
    }

    fn start_streaming_logs(
        &self,
        iterator: ServerEnd<BatchIteratorMarker>,
    ) -> Result<(), StreamError> {
        let stream_parameters = StreamParameters {
            stream_mode: Some(StreamMode::SnapshotThenSubscribe),
            data_type: Some(DataType::Logs),
            format: Some(Format::Json),
            client_selector_configuration: Some(ClientSelectorConfiguration::SelectAll(true)),
            batch_retrieval_timeout_seconds: None,
            ..Default::default()
        };
        self.accessor.stream_diagnostics(&stream_parameters, iterator).map_err(|err| {
            warn!(%err, "Failed to subscribe to isolated logs");
            StreamError::SetupSubscriptionFailed
        })?;
        Ok(())
    }
}

#[async_trait]
impl ArchiveReaderManager for IsolatedLogsProvider<'_> {
    type Error = reader::Error;

    async fn snapshot<D: diagnostics_data::DiagnosticsData + 'static>(
        &self,
    ) -> Result<Vec<Data<D>>, StreamError> {
        unimplemented!("This functionality is not yet needed.");
    }

    fn start_log_stream(
        &mut self,
    ) -> Result<
        Box<dyn FusedStream<Item = Result<LogsData, Self::Error>> + Unpin + Send>,
        StreamError,
    > {
        let (proxy, batch_iterator_server) = fidl::endpoints::create_proxy::<BatchIteratorMarker>()
            .map_err(|err| {
                warn!(%err, "Fidl error while creating proxy");
                StreamError::GenericError
            })?;
        self.start_streaming_logs(batch_iterator_server)?;
        let subscription = reader::Subscription::new(proxy);
        Ok(Box::new(subscription))
    }
}
