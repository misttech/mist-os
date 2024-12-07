// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_stream::stream;
use diagnostics_data::DiagnosticsData;
use fidl_fuchsia_diagnostics::{
    ArchiveAccessorMarker, ArchiveAccessorProxy, BatchIteratorMarker, BatchIteratorProxy,
    ClientSelectorConfiguration, Format, FormattedContent, PerformanceConfiguration, ReaderError,
    Selector, SelectorArgument, StreamMode, StreamParameters,
};
use fuchsia_async::{self as fasync, DurationExt, TimeoutExt};
use fuchsia_component::client;
use futures::channel::mpsc;
use futures::prelude::*;
use futures::sink::SinkExt;
use futures::stream::FusedStream;
use pin_project::pin_project;
use serde::Deserialize;
use std::future::ready;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use thiserror::Error;
use zx::{self as zx, MonotonicDuration};

pub use diagnostics_data::{Data, Inspect, Logs, Severity};
pub use diagnostics_hierarchy::{hierarchy, DiagnosticsHierarchy, Property};

const RETRY_DELAY_MS: i64 = 300;

#[cfg(fuchsia_api_level_at_least = "HEAD")]
const FORMAT: Format = Format::Cbor;
#[cfg(fuchsia_api_level_less_than = "HEAD")]
const FORMAT: Format = Format::Json;

/// Errors that this library can return
#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to connect to the archive accessor")]
    ConnectToArchive(#[source] anyhow::Error),

    #[error("Failed to create the BatchIterator channel ends")]
    CreateIteratorProxy(#[source] fidl::Error),

    #[error("Failed to stream diagnostics from the accessor")]
    StreamDiagnostics(#[source] fidl::Error),

    #[error("Failed to call iterator server")]
    GetNextCall(#[source] fidl::Error),

    #[error("Received error from the GetNext response: {0:?}")]
    GetNextReaderError(ReaderError),

    #[error("Failed to read json received")]
    ReadJson(#[source] serde_json::Error),

    #[cfg(fuchsia_api_level_at_least = "HEAD")]
    #[error("Failed to read cbor received")]
    ReadCbor(#[source] serde_cbor::Error),

    #[error("Failed to parse the diagnostics data from the json received")]
    ParseDiagnosticsData(#[source] serde_json::Error),

    #[error("Failed to read vmo from the response")]
    ReadVmo(#[source] zx::Status),
}

/// An inspect tree selector for a component.
pub struct ComponentSelector {
    moniker: Vec<String>,
    tree_selectors: Vec<String>,
}

impl ComponentSelector {
    /// Create a new component event selector.
    /// By default it will select the whole tree unless tree selectors are provided.
    /// `moniker` is the realm path relative to the realm of the running component plus the
    /// component name. For example: [a, b, component].
    pub fn new(moniker: Vec<String>) -> Self {
        Self { moniker, tree_selectors: Vec::new() }
    }

    /// Select a section of the inspect tree.
    pub fn with_tree_selector(mut self, tree_selector: impl Into<String>) -> Self {
        self.tree_selectors.push(tree_selector.into());
        self
    }

    fn moniker_str(&self) -> String {
        self.moniker.join("/")
    }
}

pub trait ToSelectorArguments {
    fn to_selector_arguments(self) -> Box<dyn Iterator<Item = SelectorArgument>>;
}

impl ToSelectorArguments for String {
    fn to_selector_arguments(self) -> Box<dyn Iterator<Item = SelectorArgument>> {
        Box::new([SelectorArgument::RawSelector(self)].into_iter())
    }
}

impl ToSelectorArguments for &str {
    fn to_selector_arguments(self) -> Box<dyn Iterator<Item = SelectorArgument>> {
        Box::new([SelectorArgument::RawSelector(self.to_string())].into_iter())
    }
}

impl ToSelectorArguments for ComponentSelector {
    fn to_selector_arguments(self) -> Box<dyn Iterator<Item = SelectorArgument>> {
        let moniker = self.moniker_str();
        // If not tree selectors were provided, select the full tree.
        if self.tree_selectors.is_empty() {
            Box::new([SelectorArgument::RawSelector(format!("{}:root", moniker))].into_iter())
        } else {
            Box::new(
                self.tree_selectors
                    .into_iter()
                    .map(move |s| SelectorArgument::RawSelector(format!("{moniker}:{s}"))),
            )
        }
    }
}

impl ToSelectorArguments for Selector {
    fn to_selector_arguments(self) -> Box<dyn Iterator<Item = SelectorArgument>> {
        Box::new([SelectorArgument::StructuredSelector(self)].into_iter())
    }
}

// Before unsealing this, consider whether your code belongs in this file.
pub trait SerializableValue: private::Sealed {
    const FORMAT_OF_VALUE: Format;
}

pub trait CheckResponse: private::Sealed {
    fn has_payload(&self) -> bool;
}

// The "sealed trait" pattern.
//
// https://rust-lang.github.io/api-guidelines/future-proofing.html
mod private {
    pub trait Sealed {}
}
impl private::Sealed for serde_json::Value {}
impl private::Sealed for serde_cbor::Value {}
impl<D: DiagnosticsData> private::Sealed for Data<D> {}

impl<D: DiagnosticsData> CheckResponse for Data<D> {
    fn has_payload(&self) -> bool {
        self.payload.is_some()
    }
}

impl SerializableValue for serde_json::Value {
    const FORMAT_OF_VALUE: Format = Format::Json;
}

impl CheckResponse for serde_json::Value {
    fn has_payload(&self) -> bool {
        match self {
            serde_json::Value::Object(obj) => {
                obj.get("payload").map(|p| !matches!(p, serde_json::Value::Null)).is_some()
            }
            _ => false,
        }
    }
}

#[cfg(fuchsia_api_level_at_least = "HEAD")]
impl SerializableValue for serde_cbor::Value {
    const FORMAT_OF_VALUE: Format = Format::Cbor;
}

impl CheckResponse for serde_cbor::Value {
    fn has_payload(&self) -> bool {
        match self {
            serde_cbor::Value::Map(m) => m
                .get(&serde_cbor::Value::Text("payload".into()))
                .map(|p| !matches!(p, serde_cbor::Value::Null))
                .is_some(),
            _ => false,
        }
    }
}

/// Retry configuration for ArchiveReader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RetryConfig {
    MinSchemaCount(usize),
}

impl RetryConfig {
    pub fn always() -> Self {
        Self::MinSchemaCount(1)
    }

    pub fn never() -> Self {
        Self::MinSchemaCount(0)
    }

    fn should_retry(&self, result_count: usize) -> bool {
        match self {
            Self::MinSchemaCount(min) => *min > result_count,
        }
    }
}

/// Utility for reading inspect data of a running component using the injected Archive
/// Reader service.
#[derive(Clone)]
pub struct ArchiveReader {
    archive: Option<ArchiveAccessorProxy>,
    selectors: Vec<SelectorArgument>,
    retry_config: RetryConfig,
    timeout: Option<MonotonicDuration>,
    batch_retrieval_timeout_seconds: Option<i64>,
    max_aggregated_content_size_bytes: Option<u64>,
}

impl Default for ArchiveReader {
    fn default() -> Self {
        Self {
            timeout: None,
            selectors: vec![],
            retry_config: RetryConfig::always(),
            archive: None,
            batch_retrieval_timeout_seconds: None,
            max_aggregated_content_size_bytes: None,
        }
    }
}

impl ArchiveReader {
    /// Creates a new data fetcher with default configuration:
    ///  - Maximum retries: 2^64-1
    ///  - Timeout: Never. Use with_timeout() to set a timeout.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_archive(&mut self, archive: ArchiveAccessorProxy) -> &mut Self {
        self.archive = Some(archive);
        self
    }

    /// Requests a single component tree (or sub-tree).
    pub fn add_selector(&mut self, selector: impl ToSelectorArguments) -> &mut Self {
        self.selectors.extend(selector.to_selector_arguments());
        self
    }

    /// Requests all data for the component identified by the given moniker.
    pub fn select_all_for_moniker(&mut self, moniker: &str) -> &mut Self {
        let selector = format!("{}:root", selectors::sanitize_moniker_for_selectors(moniker));
        self.add_selector(selector)
    }

    /// Sets a custom retry configuration. By default we always retry.
    pub fn retry(&mut self, config: RetryConfig) -> &mut Self {
        self.retry_config = config;
        self
    }

    pub fn add_selectors<T, S>(&mut self, selectors: T) -> &mut Self
    where
        T: Iterator<Item = S>,
        S: ToSelectorArguments,
    {
        for selector in selectors {
            self.add_selector(selector);
        }
        self
    }

    /// Sets the maximum time to wait for a response from the Archive.
    /// Do not use in tests unless timeout is the expected behavior.
    pub fn with_timeout(&mut self, duration: MonotonicDuration) -> &mut Self {
        self.timeout = Some(duration);
        self
    }

    pub fn with_aggregated_result_bytes_limit(&mut self, limit_bytes: u64) -> &mut Self {
        self.max_aggregated_content_size_bytes = Some(limit_bytes);
        self
    }

    /// Set the maximum time to wait for a wait for a single component
    /// to have its diagnostics data "pumped".
    pub fn with_batch_retrieval_timeout_seconds(&mut self, timeout: i64) -> &mut Self {
        self.batch_retrieval_timeout_seconds = Some(timeout);
        self
    }

    /// Sets the minumum number of schemas expected in a result in order for the
    /// result to be considered a success.
    pub fn with_minimum_schema_count(&mut self, minimum_schema_count: usize) -> &mut Self {
        self.retry_config = RetryConfig::MinSchemaCount(minimum_schema_count);
        self
    }

    /// Connects to the ArchiveAccessor and returns data matching provided selectors.
    pub async fn snapshot<D>(&self) -> Result<Vec<Data<D>>, Error>
    where
        D: DiagnosticsData,
    {
        let data_future = self.snapshot_inner::<D, Data<D>>(FORMAT);
        let data = match self.timeout {
            Some(timeout) => data_future.on_timeout(timeout.after_now(), || Ok(Vec::new())).await?,
            None => data_future.await?,
        };
        Ok(data)
    }

    /// Connects to the ArchiveAccessor and returns a stream of data containing a snapshot of the
    /// current buffer in the Archivist as well as new data that arrives.
    pub fn snapshot_then_subscribe<D>(&self) -> Result<Subscription<Data<D>>, Error>
    where
        D: DiagnosticsData + 'static,
    {
        let iterator = self.batch_iterator::<D>(StreamMode::SnapshotThenSubscribe, FORMAT)?;
        Ok(Subscription::new(iterator))
    }

    /// Connects to the ArchiveAccessor and returns inspect data matching provided selectors.
    /// Returns the raw json for each hierarchy fetched.
    pub async fn snapshot_raw<D, T>(&self) -> Result<T, Error>
    where
        D: DiagnosticsData,
        T: for<'a> Deserialize<'a> + SerializableValue + From<Vec<T>> + CheckResponse,
    {
        let data_future = self.snapshot_inner::<D, T>(T::FORMAT_OF_VALUE);
        let data = match self.timeout {
            Some(timeout) => data_future.on_timeout(timeout.after_now(), || Ok(Vec::new())).await?,
            None => data_future.await?,
        };
        Ok(T::from(data))
    }

    /// Connects to the ArchiveAccessor and returns a stream of data containing a snapshot of the
    /// current buffer in the Archivist as well as new data that arrives.
    pub fn snapshot_then_subscribe_raw<D, T>(&self) -> Result<Subscription<T>, Error>
    where
        D: DiagnosticsData + 'static,
        T: for<'a> Deserialize<'a> + Send + SerializableValue + 'static,
    {
        let iterator =
            self.batch_iterator::<D>(StreamMode::SnapshotThenSubscribe, T::FORMAT_OF_VALUE)?;
        Ok(Subscription::new(iterator))
    }

    async fn snapshot_inner<D, T>(&self, format: Format) -> Result<Vec<T>, Error>
    where
        D: DiagnosticsData,
        T: for<'a> Deserialize<'a> + CheckResponse,
    {
        loop {
            let iterator = self.batch_iterator::<D>(StreamMode::Snapshot, format)?;
            let result = drain_batch_iterator::<T>(Arc::new(iterator))
                .filter_map(|value| ready(value.ok()))
                .collect::<Vec<_>>()
                .await;

            if self.retry_config.should_retry(result.len()) {
                fasync::Timer::new(fasync::MonotonicInstant::after(
                    zx::MonotonicDuration::from_millis(RETRY_DELAY_MS),
                ))
                .await;
            } else {
                return Ok(result);
            }
        }
    }

    fn batch_iterator<D>(
        &self,
        mode: StreamMode,
        format: Format,
    ) -> Result<BatchIteratorProxy, Error>
    where
        D: DiagnosticsData,
    {
        let archive = match &self.archive {
            Some(archive) => archive.clone(),
            None => client::connect_to_protocol::<ArchiveAccessorMarker>()
                .map_err(Error::ConnectToArchive)?,
        };

        let (iterator, server_end) = fidl::endpoints::create_proxy::<BatchIteratorMarker>();

        let stream_parameters = StreamParameters {
            stream_mode: Some(mode),
            data_type: Some(D::DATA_TYPE),
            format: Some(format),
            client_selector_configuration: if self.selectors.is_empty() {
                Some(ClientSelectorConfiguration::SelectAll(true))
            } else {
                Some(ClientSelectorConfiguration::Selectors(self.selectors.to_vec()))
            },
            performance_configuration: Some(PerformanceConfiguration {
                max_aggregate_content_size_bytes: self.max_aggregated_content_size_bytes,
                batch_retrieval_timeout_seconds: self.batch_retrieval_timeout_seconds,
                ..Default::default()
            }),
            ..Default::default()
        };

        archive
            .stream_diagnostics(&stream_parameters, server_end)
            .map_err(Error::StreamDiagnostics)?;
        Ok(iterator)
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    Many(Vec<T>),
    One(T),
}

fn drain_batch_iterator<T>(
    iterator: Arc<BatchIteratorProxy>,
) -> impl Stream<Item = Result<T, Error>>
where
    T: for<'a> Deserialize<'a>,
{
    stream! {
        loop {
            let next_batch = iterator
                .get_next()
                .await
                .map_err(Error::GetNextCall)?
                .map_err(Error::GetNextReaderError)?;
            if next_batch.is_empty() {
                // End of stream
                return;
            }
            for formatted_content in next_batch {
                let output: OneOrMany<T> = match formatted_content {
                    FormattedContent::Json(data) => {
                        let mut buf = vec![0; data.size as usize];
                        data.vmo.read(&mut buf, 0).map_err(Error::ReadVmo)?;
                        serde_json::from_slice(&buf).map_err(Error::ReadJson)?
                    }
                    #[cfg(fuchsia_api_level_at_least = "HEAD")]
                    FormattedContent::Cbor(vmo) => {
                        let mut buf =
                            vec![0; vmo.get_content_size().expect("Always returns Ok") as usize];
                        vmo.read(&mut buf, 0).map_err(Error::ReadVmo)?;
                        serde_cbor::from_slice(&buf).map_err(Error::ReadCbor)?
                    }
                    _ => OneOrMany::Many(vec![]),
                };

                match output {
                    OneOrMany::One(data) => yield Ok(data),
                    OneOrMany::Many(datas) => {
                        for data in datas {
                            yield Ok(data);
                        }
                    }
                }
            }
        }
    }
}

#[pin_project]
pub struct Subscription<T> {
    #[pin]
    recv: Pin<Box<dyn FusedStream<Item = Result<T, Error>> + Send>>,
    iterator: Arc<BatchIteratorProxy>,
}

const DATA_CHANNEL_SIZE: usize = 32;
const ERROR_CHANNEL_SIZE: usize = 2;

impl<T> Subscription<T>
where
    T: for<'a> Deserialize<'a> + Send + 'static,
{
    /// Creates a new subscription stream to a batch iterator.
    /// The stream will return diagnostics data structures.
    pub fn new(iterator: BatchIteratorProxy) -> Self {
        let iterator = Arc::new(iterator);
        Subscription {
            recv: Box::pin(drain_batch_iterator::<T>(iterator.clone()).fuse()),
            iterator,
        }
    }

    /// Wait for the connection with the server to be established.
    pub async fn wait_for_ready(&self) {
        self.iterator.wait_for_ready().await.expect("doesn't disconnect");
    }

    /// Splits the subscription into two separate streams: results and errors.
    pub fn split_streams(mut self) -> (SubscriptionResultsStream<T>, mpsc::Receiver<Error>) {
        let (mut errors_sender, errors) = mpsc::channel(ERROR_CHANNEL_SIZE);
        let (mut results_sender, recv) = mpsc::channel(DATA_CHANNEL_SIZE);
        let _drain_task = fasync::Task::spawn(async move {
            while let Some(result) = self.next().await {
                match result {
                    Ok(value) => results_sender.send(value).await.ok(),
                    Err(e) => errors_sender.send(e).await.ok(),
                };
            }
        });
        (SubscriptionResultsStream { recv, _drain_task }, errors)
    }
}

impl<T> Stream for Subscription<T>
where
    T: for<'a> Deserialize<'a>,
{
    type Item = Result<T, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        this.recv.poll_next(cx)
    }
}

impl<T> FusedStream for Subscription<T>
where
    T: for<'a> Deserialize<'a>,
{
    fn is_terminated(&self) -> bool {
        self.recv.is_terminated()
    }
}

#[pin_project]
pub struct SubscriptionResultsStream<T> {
    #[pin]
    recv: mpsc::Receiver<T>,
    _drain_task: fasync::Task<()>,
}

impl<T> Stream for SubscriptionResultsStream<T>
where
    T: for<'a> Deserialize<'a>,
{
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        this.recv.poll_next(cx)
    }
}

impl<T> FusedStream for SubscriptionResultsStream<T>
where
    T: for<'a> Deserialize<'a>,
{
    fn is_terminated(&self) -> bool {
        self.recv.is_terminated()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use diagnostics_assertions::assert_data_tree;
    use diagnostics_log::{Publisher, PublisherOptions};
    use fuchsia_component_test::{
        Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route,
    };
    use futures::TryStreamExt;
    use tracing::{error, info};
    use {fidl_fuchsia_diagnostics as fdiagnostics, fidl_fuchsia_logger as flogger};

    const TEST_COMPONENT_URL: &str = "#meta/inspect_test_component.cm";

    struct ComponentOptions {
        publish_n_trees: u64,
    }

    async fn start_component(opts: ComponentOptions) -> Result<RealmInstance, anyhow::Error> {
        let builder = RealmBuilder::new().await?;
        let test_component = builder
            .add_child("test_component", TEST_COMPONENT_URL, ChildOptions::new().eager())
            .await?;
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                    .from(Ref::parent())
                    .to(&test_component),
            )
            .await?;
        builder.init_mutable_config_to_empty(&test_component).await.unwrap();
        builder
            .set_config_value(&test_component, "publish_n_trees", opts.publish_n_trees.into())
            .await
            .unwrap();
        let instance = builder.build().await?;
        Ok(instance)
    }

    #[fuchsia::test]
    async fn inspect_data_for_component() -> Result<(), anyhow::Error> {
        let instance = start_component(ComponentOptions { publish_n_trees: 1 }).await?;

        let moniker = format!("realm_builder:{}/test_component", instance.root.child_name());
        let component_selector = selectors::sanitize_moniker_for_selectors(&moniker);
        let results = ArchiveReader::new()
            .add_selector(format!("{component_selector}:root"))
            .snapshot::<Inspect>()
            .await?;

        assert_eq!(results.len(), 1);
        assert_data_tree!(results[0].payload.as_ref().unwrap(), root: {
            "tree-0": 0u64,
            int: 3u64,
            "lazy-node": {
                a: "test",
                child: {
                    double: 3.25,
                },
            }
        });

        // add_selector can take either a String or a Selector.
        let lazy_property_selector = Selector {
            component_selector: Some(fdiagnostics::ComponentSelector {
                moniker_segments: Some(vec![
                    fdiagnostics::StringSelector::ExactMatch(format!(
                        "realm_builder:{}",
                        instance.root.child_name()
                    )),
                    fdiagnostics::StringSelector::ExactMatch("test_component".into()),
                ]),
                ..Default::default()
            }),
            tree_selector: Some(fdiagnostics::TreeSelector::PropertySelector(
                fdiagnostics::PropertySelector {
                    node_path: vec![
                        fdiagnostics::StringSelector::ExactMatch("root".into()),
                        fdiagnostics::StringSelector::ExactMatch("lazy-node".into()),
                    ],
                    target_properties: fdiagnostics::StringSelector::ExactMatch("a".into()),
                },
            )),
            ..Default::default()
        };
        let int_property_selector = format!("{component_selector}:root:int");
        let mut reader = ArchiveReader::new();
        reader.add_selector(int_property_selector).add_selector(lazy_property_selector);
        let response = reader.snapshot::<Inspect>().await?;

        assert_eq!(response.len(), 1);

        assert_eq!(response[0].moniker.to_string(), moniker);

        assert_data_tree!(response[0].payload.as_ref().unwrap(), root: {
            int: 3u64,
            "lazy-node": {
                a: "test"
            }
        });

        Ok(())
    }

    #[fuchsia::test]
    async fn select_all_for_moniker() {
        let instance = start_component(ComponentOptions { publish_n_trees: 1 })
            .await
            .expect("component started");

        let moniker = format!("realm_builder:{}/test_component", instance.root.child_name());
        let results = ArchiveReader::new()
            .select_all_for_moniker(&moniker)
            .snapshot::<Inspect>()
            .await
            .expect("snapshotted");

        assert_eq!(results.len(), 1);
        assert_data_tree!(results[0].payload.as_ref().unwrap(), root: {
            "tree-0": 0u64,
            int: 3u64,
            "lazy-node": {
                a: "test",
                child: {
                    double: 3.25,
                },
            }
        });
    }

    #[fuchsia::test]
    async fn timeout() -> Result<(), anyhow::Error> {
        let instance = start_component(ComponentOptions { publish_n_trees: 1 }).await?;

        let mut reader = ArchiveReader::new();
        reader
            .add_selector(format!(
                "realm_builder\\:{}/test_component:root",
                instance.root.child_name()
            ))
            .with_timeout(zx::MonotonicDuration::from_nanos(0));
        let result = reader.snapshot::<Inspect>().await;
        assert!(result.unwrap().is_empty());
        Ok(())
    }

    #[fuchsia::test]
    async fn component_selector() {
        let selector = ComponentSelector::new(vec!["a".to_string()]);
        assert_eq!(selector.moniker_str(), "a");
        let arguments: Vec<_> = selector.to_selector_arguments().collect();
        assert_eq!(arguments, vec![SelectorArgument::RawSelector("a:root".to_string())]);

        let selector =
            ComponentSelector::new(vec!["b".to_string(), "c".to_string(), "a".to_string()]);
        assert_eq!(selector.moniker_str(), "b/c/a");

        let selector = selector.with_tree_selector("root/b/c:d").with_tree_selector("root/e:f");
        let arguments: Vec<_> = selector.to_selector_arguments().collect();
        assert_eq!(
            arguments,
            vec![
                SelectorArgument::RawSelector("b/c/a:root/b/c:d".into()),
                SelectorArgument::RawSelector("b/c/a:root/e:f".into()),
            ]
        );
    }

    #[fuchsia::test]
    async fn custom_archive() {
        let proxy = spawn_fake_archive(serde_json::json!({
            "moniker": "moniker",
            "version": 1,
            "data_source": "Inspect",
            "metadata": {
              "component_url": "component-url",
              "timestamp": 0,
              "filename": "filename",
            },
            "payload": {
                "root": {
                    "x": 1,
                }
            }
        }));
        let result = ArchiveReader::new()
            .with_archive(proxy)
            .snapshot::<Inspect>()
            .await
            .expect("got result");
        assert_eq!(result.len(), 1);
        assert_data_tree!(result[0].payload.as_ref().unwrap(), root: { x: 1u64 });
    }

    #[fuchsia::test]
    async fn handles_lists_correctly_on_snapshot_raw() {
        let value = serde_json::json!({
            "moniker": "moniker",
            "version": 1,
            "data_source": "Inspect",
            "metadata": {
            "component_url": "component-url",
            "timestamp": 0,
            "filename": "filename",
            },
            "payload": {
                "root": {
                    "x": 1,
                }
            }
        });
        let proxy = spawn_fake_archive(serde_json::json!([value.clone()]));
        let mut reader = ArchiveReader::new();
        reader.with_archive(proxy);
        let json_result =
            reader.snapshot_raw::<Inspect, serde_json::Value>().await.expect("got result");
        match json_result {
            serde_json::Value::Array(values) => {
                assert_eq!(values.len(), 1);
                assert_eq!(values[0], value);
            }
            result => panic!("unexpected result: {:?}", result),
        }
        let cbor_result =
            reader.snapshot_raw::<Inspect, serde_cbor::Value>().await.expect("got result");
        match cbor_result {
            serde_cbor::Value::Array(values) => {
                assert_eq!(values.len(), 1);
                let json_result =
                    serde_cbor::value::from_value::<serde_json::Value>(values[0].to_owned())
                        .expect("Should convert cleanly to JSON");
                assert_eq!(json_result, value);
            }
            result => panic!("unexpected result: {:?}", result),
        }
    }

    #[fuchsia::test]
    async fn snapshot_then_subscribe() {
        let (_instance, publisher, reader) = init_isolated_logging().await;
        let (mut stream, _errors) =
            reader.snapshot_then_subscribe::<Logs>().expect("subscribed to logs").split_streams();
        tracing::subscriber::with_default(publisher, || {
            info!("hello from test");
            error!("error from test");
        });
        let log = stream.next().await.unwrap();
        assert_eq!(log.msg().unwrap(), "hello from test");
        let log = stream.next().await.unwrap();
        assert_eq!(log.msg().unwrap(), "error from test");
    }

    #[fuchsia::test]
    async fn snapshot_then_subscribe_raw() {
        let (_instance, publisher, reader) = init_isolated_logging().await;
        let (mut stream, _errors) = reader
            .snapshot_then_subscribe_raw::<Logs, serde_json::Value>()
            .expect("subscribed to logs")
            .split_streams();
        tracing::subscriber::with_default(publisher, || {
            info!("hello from test");
            error!("error from test");
        });
        let log = stream.next().await.unwrap();
        assert_eq!(log["payload"]["root"]["message"]["value"], "hello from test");
        let log = stream.next().await.unwrap();
        assert_eq!(log["payload"]["root"]["message"]["value"], "error from test");
    }

    #[fuchsia::test]
    async fn read_many_trees_with_filtering() {
        let instance = start_component(ComponentOptions { publish_n_trees: 2 })
            .await
            .expect("component started");

        let selector =
            format!("realm_builder\\:{}/test_component:root:tree-0", instance.root.child_name());

        let results = ArchiveReader::new()
            .add_selector(selector)
            // Only one schema since empty schemas are filtered out
            .with_minimum_schema_count(1)
            .snapshot::<Inspect>()
            .await
            .expect("snapshotted");

        assert_matches!(
            results.clone().into_iter().find(|v| v.metadata.name.as_ref() == "tree-1"),
            None
        );

        let should_have_data =
            results.into_iter().find(|v| v.metadata.name.as_ref() == "tree-0").unwrap();

        assert_data_tree!(should_have_data.payload.unwrap(), root: {
            "tree-0": 0u64,
        });
    }

    fn spawn_fake_archive(data_to_send: serde_json::Value) -> fdiagnostics::ArchiveAccessorProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<fdiagnostics::ArchiveAccessorMarker>();
        fasync::Task::spawn(async move {
            while let Some(request) = stream.try_next().await.expect("stream request") {
                match request {
                    fdiagnostics::ArchiveAccessorRequest::StreamDiagnostics {
                        result_stream,
                        ..
                    } => {
                        let data = data_to_send.clone();
                        fasync::Task::spawn(async move {
                            let mut called = false;
                            let mut stream = result_stream.into_stream();
                            while let Some(req) = stream.try_next().await.expect("stream request") {
                                match req {
                                    fdiagnostics::BatchIteratorRequest::WaitForReady {
                                        responder,
                                    } => {
                                        let _ = responder.send();
                                    }
                                    fdiagnostics::BatchIteratorRequest::GetNext { responder } => {
                                        if called {
                                            responder.send(Ok(Vec::new())).expect("send response");
                                            continue;
                                        }
                                        called = true;
                                        let content = serde_json::to_string_pretty(&data)
                                            .expect("json pretty");
                                        let vmo_size = content.len() as u64;
                                        let vmo = zx::Vmo::create(vmo_size).expect("create vmo");
                                        vmo.write(content.as_bytes(), 0).expect("write vmo");
                                        let buffer =
                                            fidl_fuchsia_mem::Buffer { vmo, size: vmo_size };
                                        responder
                                            .send(Ok(vec![fdiagnostics::FormattedContent::Json(
                                                buffer,
                                            )]))
                                            .expect("send response");
                                    }
                                    fdiagnostics::BatchIteratorRequest::_UnknownMethod {
                                        ..
                                    } => {
                                        unreachable!("Unexpected method call");
                                    }
                                }
                            }
                        })
                        .detach();
                    }
                    fdiagnostics::ArchiveAccessorRequest::_UnknownMethod { .. } => {
                        unreachable!("Unexpected method call");
                    }
                }
            }
        })
        .detach();
        proxy
    }

    async fn create_realm() -> RealmBuilder {
        let builder = RealmBuilder::new().await.expect("create realm builder");
        let archivist = builder
            .add_child("archivist", "#meta/archivist-for-embedding.cm", ChildOptions::new().eager())
            .await
            .expect("add child archivist");
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                    .capability(
                        Capability::protocol_by_name("fuchsia.tracing.provider.Registry")
                            .optional(),
                    )
                    .capability(Capability::event_stream("stopped"))
                    .capability(Capability::event_stream("capability_requested"))
                    .from(Ref::parent())
                    .to(&archivist),
            )
            .await
            .expect("added routes from parent to archivist");
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                    .from(&archivist)
                    .to(Ref::parent()),
            )
            .await
            .expect("routed LogSink from archivist to parent");
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.diagnostics.ArchiveAccessor"))
                    .from_dictionary("diagnostics-accessors")
                    .from(&archivist)
                    .to(Ref::parent()),
            )
            .await
            .expect("routed ArchiveAccessor from archivist to parent");
        builder
    }

    async fn init_isolated_logging() -> (RealmInstance, Publisher, ArchiveReader) {
        let instance = create_realm().await.build().await.unwrap();
        let log_sink_proxy =
            instance.root.connect_to_protocol_at_exposed_dir::<flogger::LogSinkMarker>().unwrap();
        let accessor_proxy = instance
            .root
            .connect_to_protocol_at_exposed_dir::<fdiagnostics::ArchiveAccessorMarker>()
            .unwrap();
        let mut reader = ArchiveReader::new();
        reader.with_archive(accessor_proxy);
        let options = PublisherOptions::default()
            .wait_for_initial_interest(false)
            .use_log_sink(log_sink_proxy);
        let publisher = Publisher::new(options).unwrap();
        (instance, publisher, reader)
    }

    #[fuchsia::test]
    fn retry_config_behavior() {
        let config = RetryConfig::MinSchemaCount(1);
        let got = 0;

        assert!(config.should_retry(got));

        let config = RetryConfig::MinSchemaCount(1);
        let got = 1;

        assert!(!config.should_retry(got));

        let config = RetryConfig::MinSchemaCount(1);
        let got = 2;

        assert!(!config.should_retry(got));

        let config = RetryConfig::MinSchemaCount(0);
        let got = 1;

        assert!(!config.should_retry(got));

        let config = RetryConfig::always();
        let got = 0;

        assert!(config.should_retry(got));

        let config = RetryConfig::never();
        let got = 0;

        assert!(!config.should_retry(got));
    }
}
