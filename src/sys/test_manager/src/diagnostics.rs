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
    fidl_fuchsia_diagnostics::{
        ArchiveAccessorProxy, BatchIteratorMarker, ClientSelectorConfiguration, DataType, Format,
        StreamMode, StreamParameters,
    },
    fidl_fuchsia_test_manager as ftest_manager, fuchsia_async as fasync,
    futures::stream::FusedStream,
    std::ops::Deref,
    tracing::warn,
};

pub(crate) struct ServeSyslogOutcome {
    /// Task serving any protocols needed to proxy logs. For example, this is populated
    /// when logs are served over overnet using DiagnosticsBridge.
    pub logs_iterator_task: Option<fasync::Task<Result<(), Error>>>,
}

/// Connect to archivist and starting serving syslog.
pub(crate) fn serve_syslog(
    accessor: ArchiveAccessorProxy,
    log_iterator: ftest_manager::LogsIterator,
) -> Result<ServeSyslogOutcome, StreamError> {
    let mut provider = IsolatedLogsProvider::new(accessor);
    let logs_iterator_task = match log_iterator {
        ftest_manager::LogsIterator::Archive(iterator) => {
            let iterator_fut = provider.run_iterator_server(iterator)?;
            Some(fasync::Task::spawn(async move {
                let _on_drop = LogOnDrop("Log iterator task dropped");
                iterator_fut.await
            }))
        }
        ftest_manager::LogsIterator::Batch(iterator) => {
            provider.start_streaming_logs(iterator)?;
            None
        }
        _ => None,
    };
    Ok(ServeSyslogOutcome { logs_iterator_task })
}

struct IsolatedLogsProvider {
    accessor: ArchiveAccessorProxy,
}

impl IsolatedLogsProvider {
    fn new(accessor: ArchiveAccessorProxy) -> Self {
        Self { accessor }
    }

    fn start_streaming_logs(
        &self,
        iterator: ServerEnd<BatchIteratorMarker>,
    ) -> Result<(), StreamError> {
        self.start_streaming(iterator, StreamMode::SnapshotThenSubscribe, DataType::Logs, None)
    }

    fn start_streaming(
        &self,
        iterator: ServerEnd<BatchIteratorMarker>,
        stream_mode: StreamMode,
        data_type: DataType,
        batch_timeout: Option<i64>,
    ) -> Result<(), StreamError> {
        let stream_parameters = StreamParameters {
            stream_mode: Some(stream_mode),
            data_type: Some(data_type),
            format: Some(Format::Json),
            client_selector_configuration: Some(ClientSelectorConfiguration::SelectAll(true)),
            batch_retrieval_timeout_seconds: batch_timeout,
            ..Default::default()
        };
        self.accessor.stream_diagnostics(&stream_parameters, iterator).map_err(|err| {
            warn!(%err, "Failed to subscribe to isolated logs");
            StreamError::SetupSubscriptionFailed
        })?;
        Ok(())
    }
}

impl Deref for IsolatedLogsProvider {
    type Target = ArchiveAccessorProxy;

    fn deref(&self) -> &Self::Target {
        &self.accessor
    }
}

#[async_trait]
impl ArchiveReaderManager for IsolatedLogsProvider {
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
