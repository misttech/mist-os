// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_stream::stream;
use bitflags::bitflags;
use diagnostics_data::{DiagnosticsData, Metadata, MetadataError};
use fidl_fuchsia_diagnostics::{
    ArchiveAccessorMarker, ArchiveAccessorProxy, BatchIteratorMarker, BatchIteratorProxy,
    ClientSelectorConfiguration, Format, FormattedContent, PerformanceConfiguration, ReaderError,
    Selector, SelectorArgument, StreamMode, StreamParameters,
};
use fuchsia_async::{self as fasync, DurationExt, TimeoutExt};
use fuchsia_component::client;
use fuchsia_sync::Mutex;
use fuchsia_zircon::{self as zx, Duration, DurationNum};
use futures::{channel::mpsc, prelude::*, sink::SinkExt, stream::FusedStream};
use pin_project::pin_project;
use serde::Deserialize;
use std::{
    future::ready,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use thiserror::Error;

pub use diagnostics_data::{Data, Inspect, Logs, Severity};
pub use diagnostics_hierarchy::{hierarchy, DiagnosticsHierarchy, Property};

const RETRY_DELAY_MS: i64 = 300;

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
    fn was_fully_filtered(&self) -> bool;
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

    fn was_fully_filtered(&self) -> bool {
        self.metadata
            .errors()
            .map(|errors| {
                errors
                    .iter()
                    .filter_map(|error| error.message())
                    .any(|msg| msg.contains("Inspect hierarchy was fully filtered"))
            })
            .unwrap_or(false)
    }
}

impl SerializableValue for serde_json::Value {
    const FORMAT_OF_VALUE: Format = Format::Json;
}

const FULLY_FILTERED_MSG: &str = "Inspect hierarchy was fully filtered";

impl CheckResponse for serde_json::Value {
    fn has_payload(&self) -> bool {
        match self {
            serde_json::Value::Object(obj) => {
                obj.get("payload").map(|p| !matches!(p, serde_json::Value::Null)).is_some()
            }
            _ => false,
        }
    }

    fn was_fully_filtered(&self) -> bool {
        self.as_object()
            .and_then(|obj| obj.get("metadata"))
            .and_then(|metadata| metadata.as_object())
            .and_then(|metadata| metadata.get("errors"))
            .and_then(|errors| errors.as_array())
            .map(|errors| {
                errors
                    .iter()
                    .filter_map(|error| error.as_object())
                    .filter_map(|error| error.get("message"))
                    .filter_map(|msg| msg.as_str())
                    .any(|message| message.starts_with(FULLY_FILTERED_MSG))
            })
            .unwrap_or(false)
    }
}

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

    fn was_fully_filtered(&self) -> bool {
        let this = match self {
            serde_cbor::Value::Map(m) => m,
            _ => unreachable!("Only objects are expected"),
        };
        let metadata = match this.get(&serde_cbor::Value::Text("metadata".into())) {
            Some(serde_cbor::Value::Map(m)) => m,
            _ => unreachable!("All objects have a metadata field"),
        };
        let errors = match metadata.get(&serde_cbor::Value::Text("errors".into())) {
            None => return false, // if no errors, then we can't possibly be fully filtered.
            Some(serde_cbor::Value::Array(a)) => a,
            _ => unreachable!("We either get an array or we get nothing"),
        };
        errors
            .iter()
            .filter_map(|error| match error {
                serde_cbor::Value::Map(m) => Some(m),
                _ => None,
            })
            .filter_map(|error| error.get(&serde_cbor::Value::Text("message".into())))
            .filter_map(|msg| match msg {
                serde_cbor::Value::Text(s) => Some(s),
                _ => None,
            })
            .any(|message| message.starts_with(FULLY_FILTERED_MSG))
    }
}

bitflags! {
    /// Retry configuration for ArchiveReader.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct RetryConfig: u8 {
        /// ArchiveReader will retry on empty responses.
        const EMPTY = 0b00000001;
        /// ArchiveReader will retry when the returned hierarchy has been fully filtered.
        const FULLY_FILTERED = 0x00000010;
    }
}

impl RetryConfig {
    pub fn always() -> Self {
        Self::all()
    }

    pub fn never() -> Self {
        Self::empty()
    }

    fn on_empty(&self) -> bool {
        self.intersects(Self::EMPTY)
    }

    fn on_fully_filtered(&self) -> bool {
        self.intersects(Self::FULLY_FILTERED)
    }
}

/// Utility for reading inspect data of a running component using the injected Archive
/// Reader service.
#[derive(Clone)]
pub struct ArchiveReader {
    archive: Arc<Mutex<Option<ArchiveAccessorProxy>>>,
    selectors: Vec<SelectorArgument>,
    retry_config: RetryConfig,
    minimum_schema_count: usize,
    timeout: Option<Duration>,
    batch_retrieval_timeout_seconds: Option<i64>,
    max_aggregated_content_size_bytes: Option<u64>,
}

impl ArchiveReader {
    /// Creates a new data fetcher with default configuration:
    ///  - Maximum retries: 2^64-1
    ///  - Timeout: Never. Use with_timeout() to set a timeout.
    pub fn new() -> Self {
        Self {
            timeout: None,
            selectors: vec![],
            retry_config: RetryConfig::always(),
            archive: Arc::new(Mutex::new(None)),
            minimum_schema_count: 1,
            batch_retrieval_timeout_seconds: None,
            max_aggregated_content_size_bytes: None,
        }
    }

    pub fn with_archive(&mut self, archive: ArchiveAccessorProxy) -> &mut Self {
        {
            let mut arc = self.archive.lock();
            *arc = Some(archive);
        }
        self
    }

    /// Requests a single component tree (or sub-tree).
    pub fn add_selector(&mut self, selector: impl ToSelectorArguments) -> &mut Self {
        self.selectors.extend(selector.to_selector_arguments());
        self
    }

    /// Requests all data for the component identified by the given moniker.
    pub fn select_all_for_moniker(&mut self, moniker: &str) -> &mut Self {
        let selector = format!("{}:root", selectors::sanitize_moniker_for_selectors(&moniker));
        self.add_selector(selector)
    }

    /// Sets a custom retry configuration. By default we always retry.
    pub fn retry(&mut self, config: RetryConfig) -> &mut Self {
        self.retry_config = config;
        self
    }

    // TODO(b/308979621): soft-migrate and remove.
    #[doc(hidden)]
    pub fn retry_if_empty(&mut self, enable: bool) -> &mut Self {
        if enable {
            self.retry_config = self.retry_config | RetryConfig::EMPTY;
        } else {
            self.retry_config = self.retry_config - RetryConfig::EMPTY;
        }
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
    pub fn with_timeout(&mut self, duration: Duration) -> &mut Self {
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
        self.minimum_schema_count = minimum_schema_count;
        self
    }

    /// Connects to the ArchiveAccessor and returns data matching provided selectors.
    pub async fn snapshot<D>(&self) -> Result<Vec<Data<D>>, Error>
    where
        D: DiagnosticsData,
    {
        let data_future = self.snapshot_inner::<D, Data<D>>(Format::Cbor);
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
        let iterator = self.batch_iterator::<D>(StreamMode::SnapshotThenSubscribe, Format::Cbor)?;
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
    pub fn snapshot_then_subscribe_raw<D, T: SerializableValue + 'static>(
        &self,
    ) -> Result<Subscription<T>, Error>
    where
        D: DiagnosticsData + 'static,
        T: for<'a> Deserialize<'a> + Send,
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
            let result = drain_batch_iterator::<T>(iterator)
                .filter_map(|value| ready(value.ok()))
                .collect::<Vec<_>>()
                .await;

            if (self.retry_config.on_empty() && result.len() < self.minimum_schema_count)
                || (self.retry_config.on_fully_filtered()
                    && result
                        .iter()
                        .any(|entry| !entry.has_payload() && entry.was_fully_filtered()))
            {
                fasync::Timer::new(fasync::Time::after(RETRY_DELAY_MS.millis())).await;
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
        // TODO(https://fxbug.dev/42135966) this should be done in an ArchiveReaderBuilder -> Reader init
        let mut archive = self.archive.lock();
        if archive.is_none() {
            *archive = Some(
                client::connect_to_protocol::<ArchiveAccessorMarker>()
                    .map_err(Error::ConnectToArchive)?,
            )
        }

        let archive = archive.as_ref().unwrap();

        let (iterator, server_end) = fidl::endpoints::create_proxy::<BatchIteratorMarker>()
            .map_err(Error::CreateIteratorProxy)?;

        let mut stream_parameters = StreamParameters::default();
        stream_parameters.stream_mode = Some(mode);
        stream_parameters.data_type = Some(D::DATA_TYPE);
        stream_parameters.format = Some(format);

        stream_parameters.client_selector_configuration = if self.selectors.is_empty() {
            Some(ClientSelectorConfiguration::SelectAll(true))
        } else {
            Some(ClientSelectorConfiguration::Selectors(self.selectors.iter().cloned().collect()))
        };

        stream_parameters.performance_configuration = Some(PerformanceConfiguration {
            max_aggregate_content_size_bytes: self.max_aggregated_content_size_bytes,
            batch_retrieval_timeout_seconds: self.batch_retrieval_timeout_seconds,
            ..Default::default()
        });

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

fn drain_batch_iterator<T>(iterator: BatchIteratorProxy) -> impl Stream<Item = Result<T, Error>>
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
        Subscription { recv: Box::pin(drain_batch_iterator::<T>(iterator).fuse()) }
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
    use diagnostics_assertions::assert_data_tree;
    use diagnostics_log::{Publisher, PublisherOptions};
    use fidl_fuchsia_diagnostics as fdiagnostics;
    use fidl_fuchsia_logger as flogger;
    use fuchsia_component_test::{
        Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route,
    };
    use fuchsia_zircon as zx;
    use futures::TryStreamExt;
    use tracing::{error, info};

    const TEST_COMPONENT_URL: &str =
        "fuchsia-pkg://fuchsia.com/diagnostics-reader-tests#meta/inspect_test_component.cm";

    async fn start_component() -> Result<RealmInstance, anyhow::Error> {
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
        let instance = builder.build().await?;
        Ok(instance)
    }

    #[fuchsia::test]
    async fn inspect_data_for_component() -> Result<(), anyhow::Error> {
        let instance = start_component().await?;

        let moniker = format!("realm_builder:{}/test_component", instance.root.child_name());
        let component_selector = selectors::sanitize_moniker_for_selectors(&moniker);
        let results = ArchiveReader::new()
            .add_selector(format!("{component_selector}:root"))
            .snapshot::<Inspect>()
            .await?;

        assert_eq!(results.len(), 1);
        assert_data_tree!(results[0].payload.as_ref().unwrap(), root: {
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

        assert_eq!(response[0].metadata.component_url, Some(TEST_COMPONENT_URL.to_string()));
        assert_eq!(response[0].moniker, moniker);

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
        let instance = start_component().await.expect("started component");

        let moniker = format!("realm_builder:{}/test_component", instance.root.child_name());
        let results = ArchiveReader::new()
            .select_all_for_moniker(&moniker)
            .snapshot::<Inspect>()
            .await
            .expect("snapshotted");

        assert_eq!(results.len(), 1);
        assert_data_tree!(results[0].payload.as_ref().unwrap(), root: {
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
        let instance = start_component().await?;

        let mut reader = ArchiveReader::new();
        reader
            .add_selector(format!(
                "realm_builder\\:{}/test_component:root",
                instance.root.child_name()
            ))
            .with_timeout(0.nanos());
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

    fn spawn_fake_archive(data_to_send: serde_json::Value) -> fdiagnostics::ArchiveAccessorProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<fdiagnostics::ArchiveAccessorMarker>()
                .expect("create proxy");
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
                            let mut stream = result_stream.into_stream().expect("into stream");
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
                                        let vmo =
                                            zx::Vmo::create(vmo_size as u64).expect("create vmo");
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
        return proxy;
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
                    .capability(Capability::protocol_by_name("fuchsia.diagnostics.ArchiveAccessor"))
                    .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                    .from(&archivist)
                    .to(Ref::parent()),
            )
            .await
            .expect("added routes from archivist to parent");
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
}
