// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! # Data Structure and Algorithm Overview
//!
//! Cobalt is organized into Projects, each of which contains several Metrics.
//!
//! [`MetricConfig`] - defined in src/diagnostics/lib/sampler-config/src/lib.rs
//! This is deserialized from Sampler config files or created by FIRE by interpolating
//! component information into FIRE config files. It contains
//!
//!  - selectors: SelectorList
//!  - metric_id
//!  - metric_type: DataType
//!  - event_codes: Vec<u32>
//!  - upload_once boolean
//!
//! **NOTE:** Multiple selectors can be provided in a single metric. Only one selector is
//! expected to fetch/match data. When one does, the other selectors will be disabled for
//! efficiency.
//!
//! [`ProjectConfig`] - defined in src/diagnostics/lib/sampler-config/src/lib.rs
//! This encodes the contents of a single config file:
//!
//!  - project_id
//!  - customer_id (defaults to 1)
//!  - poll_rate_sec
//!  - metrics: Vec<MetricConfig>
//!
//! [`SamplerConfig`] - defined in src/diagnostics/lib/sampler-config/src/lib.rs
//! The entire config for Sampler. Contains
//!
//!  - list of ProjectConfig
//!  - minimum sample rate
//!
//! [`ProjectSampler`] - defined in src/diagnostics/sampler/src/executor.rs
//! This contains
//!
//!  - several MetricConfig's
//!  - an ArchiveReader configured with all active selectors
//!  - a cache of previous Diagnostic values, indexed by selector strings
//!  - FIDL proxies for Cobalt and MetricEvent loggers
//!     - these loggers are configured with project_id and customer_id
//!  - Poll rate
//!  - Inspect stats (struct ProjectSamplerStats)
//!
//! [`ProjectSampler`] is stored in:
//!
//!  - [`TaskCancellation`]:     execution_context: fasync::Task<Vec<ProjectSampler>>,
//!  - [`RebootSnapshotProcessor`]:    project_samplers: Vec<ProjectSampler>,
//!  - [`SamplerExecutor`]:     project_samplers: Vec<ProjectSampler>,
//!  - [`ProjectSamplerTaskExit::RebootTriggered(ProjectSampler)`],
//!
//! [`SamplerExecutor`] (defined in executor.rs) is built from a single [`SamplerConfig`].
//! [`SamplerExecutor`] contains
//!
//!  - a list of ProjectSamplers
//!  - an Inspect stats structure
//!
//! [`SamplerExecutor`] only has one member function execute() which calls spawn() on each
//! project sampler, passing it a receiver-oneshot to cancel it. The collection of
//! oneshot-senders and spawned-tasks builds the returned TaskCancellation.
//!
//! [`TaskCancellation`] is then passed to a reboot_watcher (in src/diagnostics/sampler/src/lib.rs)
//! which does nothing until the reboot service either closes (calling
//! run_without_cancellation()) or sends a message (calling perform_reboot_cleanup()).
//!
//! [`ProjectSampler`] calls fasync::Task::spawn to create a task that starts a timer, then loops
//! listening to that timer and to the reboot_oneshot. When the timer triggers, it calls
//! self.process_next_snapshot(). If the reboot oneshot arrives, the task returns
//! [`ProjectSamplerTaskExit::RebootTriggered(self)`].
//!
//!
//! **NOTE:** Selectors are expected to match one value each. Wildcards are only allowed in the
//! moniker segment of a selector when it is a driver in the driver collections.
//!
//! Selectors will become unused, either because of upload_once, or because data was found by a
//! different selector. Rather than implement deletion in the vecs,
//! which would add lots of bug surface and maintenance debt, each selector is an Option<> so that
//! selectors can be deleted/disabled without changing the rest of the data structure.
//! Once all Diagnostic data is processed, the structure is rebuilt if any selectors
//! have been disabled; rebuilding less often would be
//! premature optimization at this point.
//!
//! perform_reboot_cleanup() builds an ArchiveReader for [`RebootSnapshotProcessor`].
//! When not rebooting, each project fetches its own data from Archivist.

use crate::diagnostics::*;
use anyhow::{format_err, Context, Error};
use diagnostics_data::{Data, InspectHandleName};
use diagnostics_hierarchy::{
    ArrayContent, DiagnosticsHierarchy, ExponentialHistogram, LinearHistogram, Property,
    SelectResult,
};
use diagnostics_reader::{ArchiveReader, Inspect, RetryConfig};
use fidl_fuchsia_metrics::{
    HistogramBucket, MetricEvent, MetricEventLoggerFactoryMarker, MetricEventLoggerFactoryProxy,
    MetricEventLoggerProxy, MetricEventPayload, ProjectSpec,
};
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_inspect::{self as inspect, NumericProperty};
use fuchsia_inspect_derive::WithInspect;
use futures::channel::oneshot;
use futures::future::join_all;
use futures::stream::FuturesUnordered;
use futures::{select, StreamExt};
use moniker::ExtendedMoniker;
use sampler_config::{DataType, MetricConfig, ProjectConfig, SamplerConfig, SelectorList};
use selectors::SelectorExt;
use std::cell::{Ref, RefCell, RefMut};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

/// An event to be logged to the cobalt logger. Events are generated first,
/// then logged. (This permits unit-testing the code that generates events from
/// Diagnostic data.)
type EventToLog = (u32, MetricEvent);

pub struct TaskCancellation {
    senders: Vec<oneshot::Sender<()>>,
    _sampler_executor_stats: Arc<SamplerExecutorStats>,
    execution_context: fasync::Task<Vec<ProjectSampler>>,
}

impl TaskCancellation {
    /// It's possible the reboot register goes down. If that
    /// happens, we want to continue driving execution with
    /// no consideration for cancellation. This does that.
    pub async fn run_without_cancellation(self) {
        self.execution_context.await;
    }

    pub async fn perform_reboot_cleanup(self) {
        // Let every sampler know that a reboot is pending and they should exit.
        self.senders.into_iter().for_each(|sender| {
            sender
                .send(())
                .unwrap_or_else(|err| warn!("Failed to send reboot over oneshot: {:?}", err))
        });

        // Get the most recently updated project samplers from all the futures. They hold the
        // cache with the most recent values for all the metrics we need to sample and diff!
        let project_samplers: Vec<ProjectSampler> = self.execution_context.await;

        let mut reader = ArchiveReader::new();

        for project_sampler in &project_samplers {
            for metric in &project_sampler.metrics {
                for selector in metric.borrow().selectors.iter().flatten() {
                    reader.add_selector(selector.selector_string.as_str());
                }
            }
        }

        reader.retry(RetryConfig::never());

        let mut reboot_processor = RebootSnapshotProcessor { reader, project_samplers };
        // Log errors encountered in final snapshot, but always swallow errors so we can gracefully
        // notify RebootMethodsWatcherRegister that we yield our remaining time.
        reboot_processor
            .process_reboot_sample()
            .await
            .unwrap_or_else(|e| warn!("Reboot snapshot failed! {:?}", e));
    }
}

struct RebootSnapshotProcessor {
    /// Reader constructed from the union of selectors
    /// for every [`ProjectSampler`] config.
    reader: ArchiveReader,
    /// Vector of mutable [`ProjectSampler`] objects that will
    /// process their final samples.
    project_samplers: Vec<ProjectSampler>,
}

impl RebootSnapshotProcessor {
    pub async fn process_reboot_sample(&mut self) -> Result<(), Error> {
        let snapshot_data = self.reader.snapshot::<Inspect>().await?;
        for data_packet in snapshot_data {
            let moniker = data_packet.moniker;
            match data_packet.payload {
                None => {
                    process_schema_errors(&data_packet.metadata.errors, &moniker);
                }
                Some(payload) => {
                    self.process_single_payload(payload, &data_packet.metadata.name, &moniker).await
                }
            }
        }
        Ok(())
    }

    async fn process_single_payload(
        &mut self,
        hierarchy: DiagnosticsHierarchy<String>,
        inspect_handle_name: &InspectHandleName,
        moniker: &ExtendedMoniker,
    ) {
        let mut projects = self
            .project_samplers
            .iter_mut()
            .filter(|p| {
                p.filter_metrics_by_moniker_and_tree_name(moniker, inspect_handle_name.as_ref())
                    .next()
                    .is_some()
            })
            .peekable();
        if projects.peek().is_none() {
            warn!(
                %moniker,
                tree_name = inspect_handle_name.as_ref(),
                "no metrics found for moniker and tree_name combination"
            );
            return;
        }

        for project_sampler in projects {
            // If processing the final sample failed, just log the
            // error and proceed, everything's getting shut down
            // soon anyway.
            let maybe_err = match project_sampler.process_component_data(
                &hierarchy,
                inspect_handle_name,
                moniker,
            ) {
                Err(err) => Some(err),
                Ok((_selector_changes, events_to_log)) => {
                    project_sampler.log_events(events_to_log).await.err()
                }
            };
            if let Some(err) = maybe_err {
                warn!(?err, "A project sampler failed to process a reboot sample");
            }
        }
    }
}

/// Owner of the sampler execution context.
pub struct SamplerExecutor {
    project_samplers: Vec<ProjectSampler>,
    sampler_executor_stats: Arc<SamplerExecutorStats>,
}

impl SamplerExecutor {
    /// Instantiate connection to the cobalt logger and map ProjectConfigurations
    /// to [`ProjectSampler`] plans.
    pub async fn new(sampler_config: Arc<SamplerConfig>) -> Result<Self, Error> {
        let metric_logger_factory: Arc<MetricEventLoggerFactoryProxy> = Arc::new(
            connect_to_protocol::<MetricEventLoggerFactoryMarker>()
                .context("Failed to connect to the Metric LoggerFactory")?,
        );

        let minimum_sample_rate_sec = sampler_config.minimum_sample_rate_sec;

        let sampler_executor_stats = Arc::new(
            SamplerExecutorStats::new()
                .with_inspect(inspect::component::inspector().root(), "sampler_executor_stats")
                .unwrap_or_else(|err| {
                    warn!(?err, "Failed to attach inspector to SamplerExecutorStats struct");
                    SamplerExecutorStats::default()
                }),
        );

        sampler_executor_stats
            .total_project_samplers_configured
            .add(sampler_config.project_configs.len() as u64);

        let mut project_to_stats_map: HashMap<u32, Arc<ProjectSamplerStats>> = HashMap::new();
        // TODO(https://fxbug.dev/42118220): Create only one ArchiveReader for each unique poll rate so we
        // can avoid redundant snapshots.
        let project_sampler_futures =
            sampler_config.project_configs.iter().cloned().map(|project_config| {
                let project_sampler_stats =
                    project_to_stats_map.entry(project_config.project_id).or_insert_with(|| {
                        Arc::new(
                            ProjectSamplerStats::new()
                                .with_inspect(
                                    &sampler_executor_stats.inspect_node,
                                    format!("project_{:?}", project_config.project_id,),
                                )
                                .unwrap_or_else(|err| {
                                    warn!(
                                        ?err,
                                        "Failed to attach inspector to ProjectSamplerStats struct"
                                    );
                                    ProjectSamplerStats::default()
                                }),
                        )
                    });
                ProjectSampler::new(
                    project_config,
                    metric_logger_factory.clone(),
                    minimum_sample_rate_sec,
                    project_sampler_stats.clone(),
                )
            });

        let mut project_samplers: Vec<ProjectSampler> = Vec::new();
        for project_sampler in join_all(project_sampler_futures).await.into_iter() {
            match project_sampler {
                Ok(project_sampler) => project_samplers.push(project_sampler),
                Err(e) => {
                    warn!("ProjectSampler construction failed: {:?}", e);
                }
            }
        }
        Ok(SamplerExecutor { project_samplers, sampler_executor_stats })
    }

    /// Turn each [`ProjectSampler`] plan into an [`fasync::Task`] which executes its associated plan,
    /// and process errors if any tasks exit unexpectedly.
    pub fn execute(self) -> TaskCancellation {
        // Take ownership of the inspect struct so we can give it to the execution context. We do this
        // so that the execution context can return the struct when it's halted by reboot, which allows inspect
        // properties to survive through the reboot flow.
        let task_cancellation_owned_stats = self.sampler_executor_stats.clone();
        let execution_context_owned_stats = self.sampler_executor_stats.clone();

        let (senders, mut spawned_tasks): (Vec<oneshot::Sender<()>>, FuturesUnordered<_>) = self
            .project_samplers
            .into_iter()
            .map(|project_sampler| {
                let (sender, receiver) = oneshot::channel::<()>();
                (sender, project_sampler.spawn(receiver))
            })
            .unzip();

        let execution_context = fasync::Task::local(async move {
            let mut healthily_exited_samplers = Vec::new();
            while let Some(sampler_result) = spawned_tasks.next().await {
                match sampler_result {
                    Err(e) => {
                        // TODO(https://fxbug.dev/42118220): Consider restarting the failed sampler depending on
                        // failure mode.
                        warn!("A spawned sampler has failed: {:?}", e);
                        execution_context_owned_stats.errorfully_exited_samplers.add(1);
                    }
                    Ok(ProjectSamplerTaskExit::RebootTriggered(sampler)) => {
                        healthily_exited_samplers.push(sampler);
                        execution_context_owned_stats.reboot_exited_samplers.add(1);
                    }
                    Ok(ProjectSamplerTaskExit::WorkCompleted) => {
                        info!("A sampler completed its workload, and exited.");
                        execution_context_owned_stats.healthily_exited_samplers.add(1);
                    }
                }
            }

            healthily_exited_samplers
        });

        TaskCancellation {
            execution_context,
            senders,
            _sampler_executor_stats: task_cancellation_owned_stats,
        }
    }
}

pub struct ProjectSampler {
    archive_reader: ArchiveReader,
    /// The metrics used by this Project.
    metrics: Vec<RefCell<MetricConfig>>,
    /// Cache from Inspect selector to last sampled property. This is the selector from
    /// [`MetricConfig`]; it may contain wildcards.
    metric_cache: RefCell<HashMap<MetricCacheKey, Property>>,
    /// Cobalt logger proxy using this ProjectSampler's project id. It's an Option so it doesn't
    /// have to be created for unit tests; it will always be Some() outside unit tests.
    metric_loggers: HashMap<u32, MetricEventLoggerProxy>,
    /// The frequency with which we snapshot Inspect properties
    /// for this project.
    poll_rate_sec: i64,
    /// Inspect stats on a node namespaced by this project's associated id.
    /// It's an arc since a single project can have multiple samplers at
    /// different frequencies, but we want a single project to have one node.
    project_sampler_stats: Arc<ProjectSamplerStats>,
    /// The id of the project.
    /// Project ID that metrics are being sampled and forwarded on behalf of.
    project_id: u32,
    /// Records whether there are any known selectors left.
    /// Reset in rebuild_selector_data_structures().
    all_done: bool,
}

#[derive(Debug, Eq, Hash, PartialEq)]
struct MetricCacheKey {
    handle_name: InspectHandleName,
    selector: String,
}

pub enum ProjectSamplerTaskExit {
    /// The [`ProjectSampler`] processed a reboot signal on its oneshot, and yielded
    /// to the final-snapshot.
    RebootTriggered(ProjectSampler),
    /// The [`ProjectSampler`] has no more work to complete; perhaps all metrics were "upload_once"?
    WorkCompleted,
}

pub enum ProjectSamplerEvent {
    TimerTriggered,
    TimerDied,
    RebootTriggered,
    RebootChannelClosed(Error),
}

/// Indicates whether a sampler in the project has been removed (set to None), in which case the
/// ArchiveAccessor should be reconfigured.
/// The selector lists may be consolidated (and thus the maps would be rebuilt), but
/// the program will run correctly whether they are or not.
#[derive(PartialEq)]
enum SnapshotOutcome {
    SelectorsChanged,
    SelectorsUnchanged,
}

impl ProjectSampler {
    pub async fn new(
        config: Arc<ProjectConfig>,
        metric_logger_factory: Arc<MetricEventLoggerFactoryProxy>,
        minimum_sample_rate_sec: i64,
        project_sampler_stats: Arc<ProjectSamplerStats>,
    ) -> Result<ProjectSampler, Error> {
        let customer_id = config.customer_id();
        let project_id = config.project_id;
        let poll_rate_sec = config.poll_rate_sec;
        if poll_rate_sec < minimum_sample_rate_sec {
            return Err(format_err!(
                concat!(
                    "Project with id: {:?} uses a polling rate:",
                    " {:?} below minimum configured poll rate: {:?}"
                ),
                project_id,
                poll_rate_sec,
                minimum_sample_rate_sec,
            ));
        }

        project_sampler_stats.project_sampler_count.add(1);
        project_sampler_stats.metrics_configured.add(config.metrics.len() as u64);

        let mut metric_loggers = HashMap::new();
        // TODO(https://fxbug.dev/42071858): we should remove this once we support batching. There should be
        // only one metric logger per ProjectSampler.
        if project_id != 0 {
            let (metric_logger_proxy, metrics_server_end) = fidl::endpoints::create_proxy();
            let project_spec = ProjectSpec {
                customer_id: Some(customer_id),
                project_id: Some(project_id),
                ..ProjectSpec::default()
            };
            metric_logger_factory
                .create_metric_event_logger(&project_spec, metrics_server_end)
                .await?
                .map_err(|e| format_err!("error response for project {}: {:?}", project_id, e))?;
            metric_loggers.insert(project_id, metric_logger_proxy);
        }
        for metric in &config.metrics {
            if let Some(metric_project_id) = metric.project_id {
                if let Entry::Vacant(entry) = metric_loggers.entry(metric_project_id) {
                    let (metric_logger_proxy, metrics_server_end) = fidl::endpoints::create_proxy();
                    let project_spec = ProjectSpec {
                        customer_id: Some(customer_id),
                        project_id: Some(metric_project_id),
                        ..ProjectSpec::default()
                    };
                    metric_logger_factory
                        .create_metric_event_logger(&project_spec, metrics_server_end)
                        .await?
                        .map_err(|e|
                            format_err!(
                                "error response for project {} while creating metric logger {}: {:?}",
                                project_id,
                                metric_project_id,
                                e
                            ))?;
                    entry.insert(metric_logger_proxy);
                }
            }
        }

        let mut project_sampler = ProjectSampler {
            project_id,
            archive_reader: ArchiveReader::new(),
            metrics: config.metrics.iter().map(|m| RefCell::new(m.clone())).collect(),
            metric_cache: RefCell::new(HashMap::new()),
            metric_loggers,
            poll_rate_sec,
            project_sampler_stats,
            all_done: true,
        };
        // Fill in archive_reader
        project_sampler.rebuild_selector_data_structures();
        Ok(project_sampler)
    }

    pub fn spawn(
        mut self,
        mut reboot_oneshot: oneshot::Receiver<()>,
    ) -> fasync::Task<Result<ProjectSamplerTaskExit, Error>> {
        fasync::Task::local(async move {
            let mut periodic_timer =
                fasync::Interval::new(zx::MonotonicDuration::from_seconds(self.poll_rate_sec));
            loop {
                let done = select! {
                    opt = periodic_timer.next() => {
                        if opt.is_some() {
                            ProjectSamplerEvent::TimerTriggered
                        } else {
                            ProjectSamplerEvent::TimerDied
                        }
                    },
                    oneshot_res = reboot_oneshot => {
                        match oneshot_res {
                            Ok(()) => {
                                ProjectSamplerEvent::RebootTriggered
                            },
                            Err(e) => {
                                ProjectSamplerEvent::RebootChannelClosed(
                                    format_err!("Oneshot closure error: {:?}", e))
                            }
                        }
                    }
                };

                match done {
                    ProjectSamplerEvent::TimerDied => {
                        return Err(format_err!(concat!(
                            "The ProjectSampler timer died, something went wrong.",
                        )));
                    }
                    ProjectSamplerEvent::RebootChannelClosed(e) => {
                        // TODO(https://fxbug.dev/42118220): Consider differentiating errors if
                        // we ever want to recover a sampler after a oneshot channel death.
                        return Err(format_err!(
                            concat!(
                                "The Reboot signaling oneshot died, something went wrong: {:?}",
                            ),
                            e
                        ));
                    }
                    ProjectSamplerEvent::RebootTriggered => {
                        // The reboot oneshot triggered, meaning it's time to perform
                        // our final snapshot. Return self to reuse most recent cache.
                        return Ok(ProjectSamplerTaskExit::RebootTriggered(self));
                    }
                    ProjectSamplerEvent::TimerTriggered => {
                        self.process_next_snapshot().await?;
                        // Check whether this sampler
                        // still needs to run (perhaps all metrics were
                        // "upload_once"?). If it doesn't, we want to be
                        // sure that it is not included in the reboot-workload.
                        if self.is_all_done() {
                            return Ok(ProjectSamplerTaskExit::WorkCompleted);
                        }
                    }
                }
            }
        })
    }

    async fn process_next_snapshot(&mut self) -> Result<(), Error> {
        let snapshot_data = self.archive_reader.snapshot::<Inspect>().await?;
        let events_to_log = self.process_snapshot(snapshot_data).await?;
        self.log_events(events_to_log).await?;
        Ok(())
    }

    async fn process_snapshot(
        &mut self,
        snapshot: Vec<Data<Inspect>>,
    ) -> Result<Vec<EventToLog>, Error> {
        let mut selectors_changed = false;
        let mut events_to_log = vec![];
        for data_packet in snapshot.iter() {
            match &data_packet.payload {
                None => {
                    process_schema_errors(&data_packet.metadata.errors, &data_packet.moniker);
                }
                Some(payload) => {
                    let (selector_outcome, mut events) = self.process_component_data(
                        payload,
                        &data_packet.metadata.name,
                        &data_packet.moniker,
                    )?;
                    if selector_outcome == SnapshotOutcome::SelectorsChanged {
                        selectors_changed = true;
                    }
                    events_to_log.append(&mut events);
                }
            }
        }
        if selectors_changed {
            self.rebuild_selector_data_structures();
        }
        Ok(events_to_log)
    }

    fn is_all_done(&self) -> bool {
        self.all_done
    }

    fn rebuild_selector_data_structures(&mut self) {
        self.archive_reader = ArchiveReader::new();
        for metric in &self.metrics {
            // TODO(https://fxbug.dev/42168860): Using Box<ParsedSelector> could reduce copying.
            let active_selectors = metric
                .borrow()
                .selectors
                .iter()
                .filter_map(|s| if s.is_some() { Some(s.as_ref().cloned()) } else { None })
                .collect::<Vec<_>>();

            for selector in &active_selectors {
                let Some(selector) = selector.as_ref() else {
                    continue;
                };
                self.archive_reader.add_selector(selector.selector_string.as_str());
                self.all_done = false;
            }
            metric.borrow_mut().selectors = SelectorList::from(active_selectors);
        }
        self.archive_reader.retry(RetryConfig::never());
    }

    fn filter_metrics_by_moniker_and_tree_name<'a>(
        &'a self,
        moniker: &'a ExtendedMoniker,
        tree_name: &'a str,
    ) -> impl Iterator<Item = &'a RefCell<MetricConfig>> {
        self.metrics.iter().filter(|metric| {
            moniker
                .match_against_selectors_and_tree_name(
                    tree_name,
                    metric
                        .borrow()
                        .selectors
                        .iter()
                        .filter_map(|s| s.as_ref())
                        .map(|s| &s.selector),
                )
                .next()
                .is_some()
        })
    }

    fn process_component_data(
        &mut self,
        payload: &DiagnosticsHierarchy,
        inspect_handle_name: &InspectHandleName,
        moniker: &ExtendedMoniker,
    ) -> Result<(SnapshotOutcome, Vec<EventToLog>), Error> {
        let filtered_metrics =
            self.filter_metrics_by_moniker_and_tree_name(moniker, inspect_handle_name.as_ref());
        let mut snapshot_outcome = SnapshotOutcome::SelectorsUnchanged;
        let mut events_to_log = vec![];
        for metric in filtered_metrics {
            let mut selector_to_keep = None;
            let project_id = metric.borrow().project_id.unwrap_or(self.project_id);
            for (selector_idx, selector) in metric.borrow().selectors.iter().enumerate() {
                // It's fine if a selector has been removed and is None.
                let Some(parsed_selector) = selector else {
                    continue;
                };
                let found_properties = diagnostics_hierarchy::select_from_hierarchy(
                    payload,
                    &parsed_selector.selector,
                )?;
                match found_properties {
                    // Maybe the data hasn't been published yet. Maybe another selector in this
                    // metric is the correct one to find the data. Either way, not-found is fine.
                    SelectResult::Properties(p) if p.is_empty() => {}
                    SelectResult::Properties(p) if p.len() == 1 => {
                        let metric_cache_key = MetricCacheKey {
                            handle_name: inspect_handle_name.clone(),
                            selector: parsed_selector.selector_string.to_string(),
                        };
                        if let Some(event) = Self::prepare_sample(
                            metric.borrow(),
                            self.metric_cache.borrow_mut(),
                            metric_cache_key,
                            p[0],
                        )? {
                            parsed_selector.increment_upload_count();
                            events_to_log.push((project_id, event));
                        }
                        selector_to_keep = Some(selector_idx);
                        break;
                    }
                    too_many => {
                        warn!(?too_many, %parsed_selector.selector_string, "Too many matches for selector")
                    }
                }
            }

            if let Some(selector_idx) = selector_to_keep {
                if Self::update_selectors_for_metric(metric.borrow_mut(), selector_idx) {
                    snapshot_outcome = SnapshotOutcome::SelectorsChanged;
                }
            }
        }
        Ok((snapshot_outcome, events_to_log))
    }

    fn update_selectors_for_metric(
        mut metric: RefMut<'_, MetricConfig>,
        selector_idx: usize,
    ) -> bool {
        if let Some(true) = metric.upload_once {
            for selector in metric.selectors.iter_mut() {
                *selector = None;
            }
            return true;
        }
        let mut deleted = false;
        for (index, selector) in metric.selectors.iter_mut().enumerate() {
            if index != selector_idx && selector.is_some() {
                *selector = None;
                deleted = true;
            }
        }
        deleted
    }

    fn prepare_sample<'a>(
        metric: Ref<'a, MetricConfig>,
        metric_cache: RefMut<'a, HashMap<MetricCacheKey, Property>>,
        metric_cache_key: MetricCacheKey,
        new_sample: &Property,
    ) -> Result<Option<MetricEvent>, Error> {
        let previous_sample_opt: Option<&Property> = metric_cache.get(&metric_cache_key);

        if let Some(payload) = process_sample_for_data_type(
            new_sample,
            previous_sample_opt,
            &metric_cache_key,
            &metric.metric_type,
        ) {
            Self::maybe_update_cache(
                metric_cache,
                new_sample,
                &metric.metric_type,
                metric_cache_key,
            );
            Ok(Some(MetricEvent {
                metric_id: metric.metric_id,
                event_codes: metric.event_codes.clone(),
                payload,
            }))
        } else {
            Ok(None)
        }
    }

    async fn log_events(&mut self, events: Vec<EventToLog>) -> Result<(), Error> {
        for (project_id, event) in events.into_iter() {
            self.metric_loggers
                .get(&project_id)
                .as_ref()
                .unwrap()
                .log_metric_events(&[event])
                .await?
                .map_err(|e| format_err!("error from cobalt: {:?}", e))?;
            self.project_sampler_stats.cobalt_logs_sent.add(1);
        }
        Ok(())
    }

    fn maybe_update_cache(
        mut cache: RefMut<'_, HashMap<MetricCacheKey, Property>>,
        new_sample: &Property,
        data_type: &DataType,
        metric_cache_key: MetricCacheKey,
    ) {
        match data_type {
            DataType::Occurrence | DataType::IntHistogram => {
                cache.insert(metric_cache_key, new_sample.clone());
            }
            DataType::Integer | DataType::String => (),
        }
    }
}

#[cfg(test)]
impl ProjectSampler {
    fn push_metric(&mut self, metric: MetricConfig) {
        self.metrics.push(RefCell::new(metric));
    }
}

fn process_sample_for_data_type(
    new_sample: &Property,
    previous_sample_opt: Option<&Property>,
    data_source: &MetricCacheKey,
    data_type: &DataType,
) -> Option<MetricEventPayload> {
    let event_payload_res = match data_type {
        DataType::Occurrence => process_occurence(new_sample, previous_sample_opt, data_source),
        DataType::IntHistogram => {
            process_int_histogram(new_sample, previous_sample_opt, data_source)
        }
        DataType::Integer => {
            // If we previously cached a metric with an int-type, log a warning and ignore it.
            // This may be a case of using a single selector for two metrics, one event count
            // and one int.
            if previous_sample_opt.is_some() {
                warn!("Sampler has erroneously cached an Int type metric: {:?}", data_source);
            }
            process_int(new_sample, data_source)
        }
        DataType::String => {
            if previous_sample_opt.is_some() {
                warn!("Sampler has erroneously cached a String type metric: {:?}", data_source);
            }
            process_string(new_sample, data_source)
        }
    };

    match event_payload_res {
        Ok(payload_opt) => payload_opt,
        Err(err) => {
            warn!(?data_source, ?err, "Failed to process Inspect property for cobalt",);
            None
        }
    }
}

/// It's possible for Inspect numerical properties to experience overflows/conversion
/// errors when being mapped to Cobalt types. Sanitize these numericals, and provide
/// meaningful errors.
fn sanitize_unsigned_numerical(diff: u64, data_source: &MetricCacheKey) -> Result<i64, Error> {
    match diff.try_into() {
        Ok(diff) => Ok(diff),
        Err(e) => Err(format_err!(
            concat!(
                "Selector used for EventCount type",
                " refered to an unsigned int property,",
                " but cobalt requires i64, and casting introduced overflow",
                " which produces a negative int: {:?}. This could be due to",
                " a single sample being larger than i64, or a diff between",
                " samples being larger than i64. Error: {:?}"
            ),
            data_source,
            e
        )),
    }
}

fn process_int_histogram(
    new_sample: &Property,
    prev_sample_opt: Option<&Property>,
    data_source: &MetricCacheKey,
) -> Result<Option<MetricEventPayload>, Error> {
    let diff = match prev_sample_opt {
        None => convert_inspect_histogram_to_cobalt_histogram(new_sample, data_source)?,
        Some(prev_sample) => {
            // If the data type changed then we just reset the cache.
            if std::mem::discriminant(new_sample) == std::mem::discriminant(prev_sample) {
                compute_histogram_diff(new_sample, prev_sample, data_source)?
            } else {
                convert_inspect_histogram_to_cobalt_histogram(new_sample, data_source)?
            }
        }
    };

    let non_empty_diff: Vec<HistogramBucket> = diff.into_iter().filter(|v| v.count != 0).collect();
    if !non_empty_diff.is_empty() {
        Ok(Some(MetricEventPayload::Histogram(non_empty_diff)))
    } else {
        Ok(None)
    }
}

fn compute_histogram_diff(
    new_sample: &Property,
    old_sample: &Property,
    data_source: &MetricCacheKey,
) -> Result<Vec<HistogramBucket>, Error> {
    let new_histogram_buckets =
        convert_inspect_histogram_to_cobalt_histogram(new_sample, data_source)?;
    let old_histogram_buckets =
        convert_inspect_histogram_to_cobalt_histogram(old_sample, data_source)?;

    if old_histogram_buckets.len() != new_histogram_buckets.len() {
        return Err(format_err!(
            concat!(
                "Selector referenced an Inspect IntArray",
                " that was specified as an IntHistogram type ",
                " but the histogram bucket count changed between",
                " samples, which is incompatible with Cobalt.",
                " Selector: {:?}, Inspect type: {}"
            ),
            data_source,
            new_sample.discriminant_name()
        ));
    }

    new_histogram_buckets
        .iter()
        .zip(old_histogram_buckets)
        .map(|(new_bucket, old_bucket)| {
            if new_bucket.count < old_bucket.count {
                return Err(format_err!(
                    concat!(
                        "Selector referenced an Inspect IntArray",
                        " that was specified as an IntHistogram type ",
                        " but at least one bucket saw the count decrease",
                        " between samples, which is incompatible with Cobalt's",
                        " need for monotonically increasing counts.",
                        " Selector: {:?}, Inspect type: {}"
                    ),
                    data_source,
                    new_sample.discriminant_name()
                ));
            }
            Ok(HistogramBucket {
                count: new_bucket.count - old_bucket.count,
                index: new_bucket.index,
            })
        })
        .collect::<Result<Vec<HistogramBucket>, Error>>()
}

fn build_cobalt_histogram(counts: impl Iterator<Item = u64>) -> Vec<HistogramBucket> {
    counts
        .enumerate()
        .map(|(index, count)| HistogramBucket { index: index as u32, count })
        .collect()
}

fn build_sparse_cobalt_histogram(
    counts: impl Iterator<Item = u64>,
    indexes: &[usize],
    size: usize,
) -> Vec<HistogramBucket> {
    let mut histogram =
        Vec::from_iter((0..size).map(|index| HistogramBucket { index: index as u32, count: 0 }));
    for (index, count) in indexes.iter().zip(counts) {
        histogram[*index].count = count;
    }
    histogram
}

fn convert_inspect_histogram_to_cobalt_histogram(
    inspect_histogram: &Property,
    data_source: &MetricCacheKey,
) -> Result<Vec<HistogramBucket>, Error> {
    macro_rules! err {($($message:expr),+) => {
        Err(format_err!(
            concat!($($message),+ , " Selector: {:?}, Inspect type: {}"),
            data_source,
            inspect_histogram.discriminant_name()
        ))
    }}

    let sanitize_size = |size: usize| -> Result<(), Error> {
        if size > u32::MAX as usize {
            return err!(
                "Selector referenced an Inspect array",
                " that was specified as a histogram type ",
                " but contained an index too large for a u32."
            );
        }
        Ok(())
    };

    let sanitize_indexes = |indexes: &[usize], size: usize| -> Result<(), Error> {
        for index in indexes.iter() {
            if *index >= size {
                return err!(
                    "Selector referenced an Inspect array",
                    " that was specified as a histogram type ",
                    " but contained an invalid index."
                );
            }
        }
        Ok(())
    };

    let sanitize_counts = |counts: &[i64]| -> Result<(), Error> {
        for count in counts.iter() {
            if *count < 0 {
                return err!(
                    "Selector referenced an Inspect IntArray",
                    " that was specified as an IntHistogram type ",
                    " but a bucket contained a negative count. This",
                    " is incompatible with Cobalt histograms which only",
                    " support positive histogram counts."
                );
            }
        }
        Ok(())
    };

    let histogram = match inspect_histogram {
        Property::IntArray(
            _,
            ArrayContent::LinearHistogram(LinearHistogram { counts, indexes, size, .. }),
        )
        | Property::IntArray(
            _,
            ArrayContent::ExponentialHistogram(ExponentialHistogram {
                counts, indexes, size, ..
            }),
        ) => {
            sanitize_size(*size)?;
            sanitize_counts(counts)?;
            match (indexes, counts) {
                (None, counts) => build_cobalt_histogram(counts.iter().map(|c| *c as u64)),
                (Some(indexes), counts) => {
                    sanitize_indexes(indexes, *size)?;
                    build_sparse_cobalt_histogram(counts.iter().map(|c| *c as u64), indexes, *size)
                }
            }
        }
        Property::UintArray(
            _,
            ArrayContent::LinearHistogram(LinearHistogram { counts, indexes, size, .. }),
        )
        | Property::UintArray(
            _,
            ArrayContent::ExponentialHistogram(ExponentialHistogram {
                counts, indexes, size, ..
            }),
        ) => {
            sanitize_size(*size)?;
            match (indexes, counts) {
                (None, counts) => build_cobalt_histogram(counts.iter().copied()),
                (Some(indexes), counts) => {
                    sanitize_indexes(indexes, *size)?;
                    build_sparse_cobalt_histogram(counts.iter().copied(), indexes, *size)
                }
            }
        }
        _ => {
            // TODO(https://fxbug.dev/42118220): Does cobalt support floors or step counts that are
            // not ints? if so, we can support that as well with double arrays if the
            // actual counts are whole numbers.
            return Err(format_err!(
                concat!(
                    "Selector referenced an Inspect property",
                    " that was specified as an IntHistogram type ",
                    " but is unable to be encoded in a cobalt HistogramBucket",
                    " vector. Selector: {:?}, Inspect type: {}"
                ),
                data_source,
                inspect_histogram.discriminant_name()
            ));
        }
    };
    Ok(histogram)
}

fn process_int(
    new_sample: &Property,
    data_source: &MetricCacheKey,
) -> Result<Option<MetricEventPayload>, Error> {
    let sampled_int = match new_sample {
        Property::Uint(_, val) => sanitize_unsigned_numerical(*val, data_source)?,
        Property::Int(_, val) => *val,
        _ => {
            return Err(format_err!(
                concat!(
                    "Selector referenced an Inspect property",
                    " that was specified as an Int type ",
                    " but is unable to be encoded in an i64",
                    " Selector: {:?}, Inspect type: {}"
                ),
                data_source,
                new_sample.discriminant_name()
            ));
        }
    };

    Ok(Some(MetricEventPayload::IntegerValue(sampled_int)))
}

fn process_string(
    new_sample: &Property,
    data_source: &MetricCacheKey,
) -> Result<Option<MetricEventPayload>, Error> {
    let sampled_string = match new_sample {
        Property::String(_, val) => val.clone(),
        _ => {
            return Err(format_err!(
                concat!(
                    "Selector referenced an Inspect property specified as String",
                    " but property is not type String.",
                    " Selector: {:?}, Inspect type: {}"
                ),
                data_source,
                new_sample.discriminant_name()
            ));
        }
    };

    Ok(Some(MetricEventPayload::StringValue(sampled_string)))
}

fn process_occurence(
    new_sample: &Property,
    prev_sample_opt: Option<&Property>,
    data_source: &MetricCacheKey,
) -> Result<Option<MetricEventPayload>, Error> {
    let diff = match prev_sample_opt {
        None => compute_initial_event_count(new_sample, data_source)?,
        Some(prev_sample) => compute_event_count_diff(new_sample, prev_sample, data_source)?,
    };

    if diff < 0 {
        return Err(format_err!(
            concat!(
                "Event count must be monotonically increasing,",
                " but we observed a negative event count diff for: {:?}"
            ),
            data_source
        ));
    }

    if diff == 0 {
        return Ok(None);
    }

    // TODO(https://fxbug.dev/42118220): Once fuchsia.cobalt is gone, we don't need to preserve
    // occurrence counts "fitting" into i64s.
    Ok(Some(MetricEventPayload::Count(diff as u64)))
}

fn compute_initial_event_count(
    new_sample: &Property,
    data_source: &MetricCacheKey,
) -> Result<i64, Error> {
    match new_sample {
        Property::Uint(_, val) => sanitize_unsigned_numerical(*val, data_source),
        Property::Int(_, val) => Ok(*val),
        _ => Err(format_err!(
            concat!(
                "Selector referenced an Inspect property",
                " that is not compatible with cached",
                " transformation to an event count.",
                " Selector: {:?}, {}"
            ),
            data_source,
            new_sample.discriminant_name()
        )),
    }
}

fn compute_event_count_diff(
    new_sample: &Property,
    old_sample: &Property,
    data_source: &MetricCacheKey,
) -> Result<i64, Error> {
    match (new_sample, old_sample) {
        // We don't need to validate that old_count and new_count are positive here.
        // If new_count was negative, and old_count was positive, then the diff would be
        // negative, which is an errorful state. It's impossible for old_count to be negative
        // as either it was the first sample which would make a negative diff which is an error,
        // or it was a negative new_count with a positive old_count, which we've already shown will
        // produce an errorful state.
        (Property::Int(_, new_count), Property::Int(_, old_count)) => Ok(new_count - old_count),
        (Property::Uint(_, new_count), Property::Uint(_, old_count)) => {
            // u64::MAX will cause sanitized_unsigned_numerical to build an
            // appropriate error message for a subtraction underflow.
            sanitize_unsigned_numerical(
                new_count.checked_sub(*old_count).unwrap_or(u64::MAX),
                data_source,
            )
        }
        // If we have a correctly typed new sample, but it didn't match either of the above cases,
        // this means the new sample changed types compared to the old sample. We should just
        // restart the cache, and treat the new sample as a first observation.
        (_, Property::Uint(_, _)) | (_, Property::Int(_, _)) => {
            warn!(
                "Inspect type of sampled data changed between samples. Restarting cache. {:?}",
                data_source
            );
            compute_initial_event_count(new_sample, data_source)
        }
        _ => Err(format_err!(
            concat!(
                "Inspect type of sampled data changed between samples",
                " to a type incompatible with event counters.",
                " Selector: {:?}, New type: {:?}"
            ),
            data_source,
            new_sample.discriminant_name()
        )),
    }
}

fn process_schema_errors(
    errors: &Option<Vec<diagnostics_data::InspectError>>,
    moniker: &ExtendedMoniker,
) {
    match errors {
        Some(errors) => {
            for error in errors {
                warn!(%moniker, ?error);
            }
        }
        None => {
            warn!(%moniker, "Encountered null payload and no errors.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_data::{InspectDataBuilder, Timestamp};
    use diagnostics_hierarchy::hierarchy;
    use fidl_fuchsia_inspect::DEFAULT_TREE_NAME;

    #[fuchsia::test]
    fn test_filter_metrics() {
        let mut sampler = ProjectSampler {
            archive_reader: ArchiveReader::new(),
            metrics: vec![],
            metric_cache: RefCell::new(HashMap::new()),
            metric_loggers: HashMap::new(),
            project_id: 1,
            poll_rate_sec: 3600,
            project_sampler_stats: Arc::new(ProjectSamplerStats::new()),
            all_done: true,
        };
        let selector_foo: String = "core/foo:[name=foo]root/path:value".to_string();
        let selector_bar: String = "core/foo:[name=bar]root/path:value".to_string();

        sampler.push_metric(MetricConfig {
            project_id: None,
            selectors: SelectorList::from(vec![sampler_config::parse_selector_for_test(
                &selector_foo,
            )]),
            metric_id: 1,
            // Occurrence type with a value of zero will not attempt to use any loggers.
            metric_type: DataType::Occurrence,
            event_codes: Vec::new(),
            // upload_once means that process_component_data will return SelectorsChanged if
            // it is found in the map.
            upload_once: Some(true),
        });
        sampler.push_metric(MetricConfig {
            project_id: None,
            selectors: SelectorList::from(vec![sampler_config::parse_selector_for_test(
                &selector_bar,
            )]),
            metric_id: 2,
            // Occurrence type with a value of zero will not attempt to use any loggers.
            metric_type: DataType::Occurrence,
            event_codes: Vec::new(),
            // upload_once means that process_component_data will return SelectorsChanged if
            // it is found in the map.
            upload_once: Some(true),
        });

        sampler.rebuild_selector_data_structures();

        let moniker = ExtendedMoniker::try_from("core/foo").unwrap();

        let filtered_metrics =
            sampler.filter_metrics_by_moniker_and_tree_name(&moniker, "foo").collect::<Vec<_>>();
        assert_eq!(1, filtered_metrics.len());
        assert_eq!(1, filtered_metrics[0].borrow().metric_id);
    }

    #[fuchsia::test]
    fn test_filter_metrics_with_wildcards() {
        let mut sampler = ProjectSampler {
            archive_reader: ArchiveReader::new(),
            metrics: vec![],
            metric_cache: RefCell::new(HashMap::new()),
            metric_loggers: HashMap::new(),
            project_id: 1,
            poll_rate_sec: 3600,
            project_sampler_stats: Arc::new(ProjectSamplerStats::new()),
            all_done: true,
        };
        let selector_foo: String = r"bootstrap/*-drivers\:*:[name=foo]root/path:value".to_string();
        let selector_bar1: String =
            r"bootstrap/*-drivers\:*:[name=bar]root/path:value1".to_string();
        let selector_bar2: String =
            r"bootstrap/*-drivers\:*:[name=bar]root/path:value2".to_string();

        sampler.push_metric(MetricConfig {
            project_id: None,
            selectors: SelectorList::from(vec![sampler_config::parse_selector_for_test(
                &selector_foo,
            )]),
            metric_id: 1,
            // Occurrence type with a value of zero will not attempt to use any loggers.
            metric_type: DataType::Occurrence,
            event_codes: Vec::new(),
            // upload_once means that process_component_data will return SelectorsChanged if
            // it is found in the map.
            upload_once: Some(true),
        });
        sampler.push_metric(MetricConfig {
            project_id: None,
            selectors: SelectorList::from(vec![sampler_config::parse_selector_for_test(
                &selector_bar1,
            )]),
            metric_id: 2,
            // Occurrence type with a value of zero will not attempt to use any loggers.
            metric_type: DataType::Occurrence,
            event_codes: Vec::new(),
            // upload_once means that process_component_data will return SelectorsChanged if
            // it is found in the map.
            upload_once: Some(true),
        });
        sampler.push_metric(MetricConfig {
            project_id: None,
            selectors: SelectorList::from(vec![sampler_config::parse_selector_for_test(
                &selector_bar2,
            )]),
            metric_id: 3,
            // Occurrence type with a value of zero will not attempt to use any loggers.
            metric_type: DataType::Occurrence,
            event_codes: Vec::new(),
            // upload_once means that process_component_data will return SelectorsChanged if
            // it is found in the map.
            upload_once: Some(true),
        });

        sampler.rebuild_selector_data_structures();

        let moniker = ExtendedMoniker::try_from("bootstrap/boot-drivers:098098").unwrap();

        let filtered_metrics =
            sampler.filter_metrics_by_moniker_and_tree_name(&moniker, "foo").collect::<Vec<_>>();
        assert_eq!(1, filtered_metrics.len());
        assert_eq!(1, filtered_metrics[0].borrow().metric_id);

        let filtered_metrics =
            sampler.filter_metrics_by_moniker_and_tree_name(&moniker, "bar").collect::<Vec<_>>();
        assert_eq!(2, filtered_metrics.len());
        assert_eq!(2, filtered_metrics[0].borrow().metric_id);
        assert_eq!(3, filtered_metrics[1].borrow().metric_id);
    }

    /// Test inserting a string into the hierarchy that requires escaping.
    #[fuchsia::test]
    fn test_process_payload_with_escapes() {
        let unescaped: String = "path/to".to_string();
        let hierarchy = hierarchy! {
            root: {
                var unescaped: {
                    value: 0,
                    "value/with:escapes": 0,
                }
            }
        };

        let mut sampler = ProjectSampler {
            archive_reader: ArchiveReader::new(),
            metrics: vec![],
            metric_cache: RefCell::new(HashMap::new()),
            metric_loggers: HashMap::new(),
            project_id: 1,
            poll_rate_sec: 3600,
            project_sampler_stats: Arc::new(ProjectSamplerStats::new()),
            all_done: true,
        };
        let selector: String = "my/component:root/path\\/to:value".to_string();
        sampler.push_metric(MetricConfig {
            project_id: None,
            selectors: SelectorList::from(vec![sampler_config::parse_selector_for_test(&selector)]),
            metric_id: 1,
            // Occurrence type with a value of zero will not attempt to use any loggers.
            metric_type: DataType::Occurrence,
            event_codes: Vec::new(),
            // upload_once means that process_component_data will return SelectorsChanged if
            // it is found in the map.
            upload_once: Some(true),
        });
        sampler.rebuild_selector_data_structures();
        match sampler.process_component_data(
            &hierarchy,
            &InspectHandleName::name(DEFAULT_TREE_NAME),
            &"my/component".try_into().unwrap(),
        ) {
            // This selector will be found and removed from the map, resulting in a
            // SelectorsChanged response.
            Ok((SnapshotOutcome::SelectorsChanged, _events)) => (),
            _ => panic!("Expecting SelectorsChanged from process_component_data."),
        }

        let selector_with_escaped_property: String =
            "my/component:root/path\\/to:value\\/with\\:escapes".to_string();
        sampler.push_metric(MetricConfig {
            project_id: None,
            selectors: SelectorList::from(vec![sampler_config::parse_selector_for_test(
                &selector_with_escaped_property,
            )]),
            metric_id: 1,
            // Occurrence type with a value of zero will not attempt to use any loggers.
            metric_type: DataType::Occurrence,
            event_codes: Vec::new(),
            // upload_once means that the method will return SelectorsChanged if it is found
            // in the map.
            upload_once: Some(true),
        });
        sampler.rebuild_selector_data_structures();
        match sampler.process_component_data(
            &hierarchy,
            &InspectHandleName::name(DEFAULT_TREE_NAME),
            &"my/component".try_into().unwrap(),
        ) {
            // This selector will be found and removed from the map, resulting in a
            // SelectorsChanged response.
            Ok((SnapshotOutcome::SelectorsChanged, _events)) => (),
            _ => panic!("Expecting SelectorsChanged from process_component_data."),
        }

        let selector_unfound: String = "my/component:root/path/to:value".to_string();
        sampler.push_metric(MetricConfig {
            project_id: None,
            selectors: SelectorList::from(vec![sampler_config::parse_selector_for_test(
                &selector_unfound,
            )]),
            metric_id: 1,
            // Occurrence type with a value of zero will not attempt to use any loggers.
            metric_type: DataType::Occurrence,
            event_codes: Vec::new(),
            // upload_once means that the method will return SelectorsChanged if it is found
            // in the map.
            upload_once: Some(true),
        });
        sampler.rebuild_selector_data_structures();
        match sampler.process_component_data(
            &hierarchy,
            &InspectHandleName::name(DEFAULT_TREE_NAME),
            &"my/component".try_into().unwrap(),
        ) {
            // This selector will not be found and removed from the map, resulting in SelectorsUnchanged.
            Ok((SnapshotOutcome::SelectorsUnchanged, _events)) => (),
            _ => panic!("Expecting SelectorsUnchanged from process_component_data."),
        }
    }

    /// Test that a decreasing occurrence type (which is not allowed) doesn't crash due to e.g.
    /// unchecked unsigned subtraction overflow.
    #[fuchsia::test]
    fn decreasing_occurrence_is_correct() {
        let big_number = Property::Uint("foo".to_string(), 5);
        let small_number = Property::Uint("foo".to_string(), 2);
        let key = MetricCacheKey {
            handle_name: InspectHandleName::name("some_file"),
            selector: "sel".to_string(),
        };

        assert_eq!(
            process_sample_for_data_type(
                &big_number,
                Some(&small_number),
                &key,
                &DataType::Occurrence
            ),
            Some(MetricEventPayload::Count(3))
        );
        assert_eq!(
            process_sample_for_data_type(
                &small_number,
                Some(&big_number),
                &key,
                &DataType::Occurrence
            ),
            None
        );
    }

    /// Test removal of selectors marked with upload_once.
    #[fuchsia::test]
    fn test_upload_once() {
        let hierarchy = hierarchy! {
            root: {
                value_one: 0,
                value_two: 1,
            }
        };

        let mut sampler = ProjectSampler {
            archive_reader: ArchiveReader::new(),
            metrics: vec![],
            metric_cache: RefCell::new(HashMap::new()),
            metric_loggers: HashMap::new(),
            project_id: 1,
            poll_rate_sec: 3600,
            project_sampler_stats: Arc::new(ProjectSamplerStats::new()),
            all_done: true,
        };
        sampler.push_metric(MetricConfig {
            project_id: None,
            selectors: SelectorList::from(vec![sampler_config::parse_selector_for_test(
                "my/component:root:value_one",
            )]),
            metric_id: 1,
            metric_type: DataType::Integer,
            event_codes: Vec::new(),
            upload_once: Some(true),
        });
        sampler.push_metric(MetricConfig {
            project_id: None,
            selectors: SelectorList::from(vec![sampler_config::parse_selector_for_test(
                "my/component:root:value_two",
            )]),
            metric_id: 2,
            metric_type: DataType::Integer,
            event_codes: Vec::new(),
            upload_once: Some(true),
        });
        sampler.rebuild_selector_data_structures();

        // Both selectors should be found and removed from the map.
        match sampler.process_component_data(
            &hierarchy,
            &InspectHandleName::name(DEFAULT_TREE_NAME),
            &"my/component".try_into().unwrap(),
        ) {
            Ok((SnapshotOutcome::SelectorsChanged, _events)) => (),
            _ => panic!("Expecting SelectorsChanged from process_component_data."),
        }

        let moniker: ExtendedMoniker = "my_component".try_into().unwrap();
        assert!(sampler
            .filter_metrics_by_moniker_and_tree_name(&moniker, DEFAULT_TREE_NAME)
            .collect::<Vec<_>>()
            .is_empty());
    }

    struct EventCountTesterParams {
        new_val: Property,
        old_val: Option<Property>,
        process_ok: bool,
        event_made: bool,
        diff: i64,
    }

    fn process_occurence_tester(params: EventCountTesterParams) {
        let data_source = MetricCacheKey {
            handle_name: InspectHandleName::name("foo.file"),
            selector: "test:root:count".to_string(),
        };
        let event_res = process_occurence(&params.new_val, params.old_val.as_ref(), &data_source);

        if !params.process_ok {
            assert!(event_res.is_err());
            return;
        }

        assert!(event_res.is_ok());

        let event_opt = event_res.unwrap();

        if !params.event_made {
            assert!(event_opt.is_none());
            return;
        }

        assert!(event_opt.is_some());
        let event = event_opt.unwrap();
        match event {
            MetricEventPayload::Count(count) => {
                assert_eq!(count, params.diff as u64);
            }
            _ => panic!("Expecting event counts."),
        }
    }

    #[fuchsia::test]
    fn test_normal_process_occurence() {
        process_occurence_tester(EventCountTesterParams {
            new_val: Property::Int("count".to_string(), 1),
            old_val: None,
            process_ok: true,
            event_made: true,
            diff: 1,
        });

        process_occurence_tester(EventCountTesterParams {
            new_val: Property::Int("count".to_string(), 1),
            old_val: Some(Property::Int("count".to_string(), 1)),
            process_ok: true,
            event_made: false,
            diff: -1,
        });

        process_occurence_tester(EventCountTesterParams {
            new_val: Property::Int("count".to_string(), 3),
            old_val: Some(Property::Int("count".to_string(), 1)),
            process_ok: true,
            event_made: true,
            diff: 2,
        });
    }

    #[fuchsia::test]
    fn test_data_type_changing_process_occurence() {
        process_occurence_tester(EventCountTesterParams {
            new_val: Property::Int("count".to_string(), 1),
            old_val: None,
            process_ok: true,
            event_made: true,
            diff: 1,
        });

        process_occurence_tester(EventCountTesterParams {
            new_val: Property::Uint("count".to_string(), 1),
            old_val: None,
            process_ok: true,
            event_made: true,
            diff: 1,
        });

        process_occurence_tester(EventCountTesterParams {
            new_val: Property::Uint("count".to_string(), 3),
            old_val: Some(Property::Int("count".to_string(), 1)),
            process_ok: true,
            event_made: true,
            diff: 3,
        });

        process_occurence_tester(EventCountTesterParams {
            new_val: Property::String("count".to_string(), "big_oof".to_string()),
            old_val: Some(Property::Int("count".to_string(), 1)),
            process_ok: false,
            event_made: false,
            diff: -1,
        });
    }

    #[fuchsia::test]
    fn test_event_count_negatives_and_overflows() {
        process_occurence_tester(EventCountTesterParams {
            new_val: Property::Int("count".to_string(), -11),
            old_val: None,
            process_ok: false,
            event_made: false,
            diff: -1,
        });

        process_occurence_tester(EventCountTesterParams {
            new_val: Property::Int("count".to_string(), 9),
            old_val: Some(Property::Int("count".to_string(), 10)),
            process_ok: false,
            event_made: false,
            diff: -1,
        });

        process_occurence_tester(EventCountTesterParams {
            new_val: Property::Uint("count".to_string(), u64::MAX),
            old_val: None,
            process_ok: false,
            event_made: false,
            diff: -1,
        });

        let i64_max_in_u64: u64 = i64::MAX.try_into().unwrap();

        process_occurence_tester(EventCountTesterParams {
            new_val: Property::Uint("count".to_string(), i64_max_in_u64 + 1),
            old_val: Some(Property::Uint("count".to_string(), 1)),
            process_ok: true,
            event_made: true,
            diff: i64::MAX,
        });

        process_occurence_tester(EventCountTesterParams {
            new_val: Property::Uint("count".to_string(), i64_max_in_u64 + 2),
            old_val: Some(Property::Uint("count".to_string(), 1)),
            process_ok: false,
            event_made: false,
            diff: -1,
        });
    }

    struct IntTesterParams {
        new_val: Property,
        process_ok: bool,
        sample: i64,
    }

    fn process_int_tester(params: IntTesterParams) {
        let data_source = MetricCacheKey {
            handle_name: InspectHandleName::name("foo.file"),
            selector: "test:root:count".to_string(),
        };
        let event_res = process_int(&params.new_val, &data_source);

        if !params.process_ok {
            assert!(event_res.is_err());
            return;
        }

        assert!(event_res.is_ok());

        let event = event_res.expect("event should be Ok").expect("event should be Some");
        match event {
            MetricEventPayload::IntegerValue(val) => {
                assert_eq!(val, params.sample);
            }
            _ => panic!("Expecting event counts."),
        }
    }

    #[fuchsia::test]
    fn test_normal_process_int() {
        process_int_tester(IntTesterParams {
            new_val: Property::Int("count".to_string(), 13),
            process_ok: true,
            sample: 13,
        });

        process_int_tester(IntTesterParams {
            new_val: Property::Int("count".to_string(), -13),
            process_ok: true,
            sample: -13,
        });

        process_int_tester(IntTesterParams {
            new_val: Property::Int("count".to_string(), 0),
            process_ok: true,
            sample: 0,
        });

        process_int_tester(IntTesterParams {
            new_val: Property::Uint("count".to_string(), 13),
            process_ok: true,
            sample: 13,
        });

        process_int_tester(IntTesterParams {
            new_val: Property::String("count".to_string(), "big_oof".to_string()),
            process_ok: false,
            sample: -1,
        });
    }

    #[fuchsia::test]
    fn test_int_edge_cases() {
        process_int_tester(IntTesterParams {
            new_val: Property::Int("count".to_string(), i64::MAX),
            process_ok: true,
            sample: i64::MAX,
        });

        process_int_tester(IntTesterParams {
            new_val: Property::Int("count".to_string(), i64::MIN),
            process_ok: true,
            sample: i64::MIN,
        });

        let i64_max_in_u64: u64 = i64::MAX.try_into().unwrap();

        process_int_tester(IntTesterParams {
            new_val: Property::Uint("count".to_string(), i64_max_in_u64),
            process_ok: true,
            sample: i64::MAX,
        });

        process_int_tester(IntTesterParams {
            new_val: Property::Uint("count".to_string(), i64_max_in_u64 + 1),
            process_ok: false,
            sample: -1,
        });
    }

    struct StringTesterParams {
        sample: Property,
        process_ok: bool,
        previous_sample: Option<Property>,
    }

    fn process_string_tester(params: StringTesterParams) {
        let metric_cache_key = MetricCacheKey {
            handle_name: InspectHandleName::name("foo.file"),
            selector: "test:root:string_val".to_string(),
        };

        let event = process_sample_for_data_type(
            &params.sample,
            params.previous_sample.as_ref(),
            &metric_cache_key,
            &DataType::String,
        );

        if !params.process_ok {
            assert!(event.is_none());
            return;
        }

        match event.unwrap() {
            MetricEventPayload::StringValue(val) => {
                assert_eq!(val.as_str(), params.sample.string().unwrap());
            }
            _ => panic!("Expecting event with StringValue."),
        }
    }

    #[fuchsia::test]
    fn test_process_string() {
        process_string_tester(StringTesterParams {
            sample: Property::String("string_val".to_string(), "Hello, world!".to_string()),
            process_ok: true,
            previous_sample: None,
        });

        // Ensure any erroneously cached values are ignored (a warning is logged in this case).

        process_string_tester(StringTesterParams {
            sample: Property::String("string_val".to_string(), "Hello, world!".to_string()),
            process_ok: true,
            previous_sample: Some(Property::String("string_val".to_string(), "Uh oh!".to_string())),
        });

        // Ensure unsupported property types are not erroneously processed.

        process_string_tester(StringTesterParams {
            sample: Property::Int("string_val".to_string(), 123),
            process_ok: false,
            previous_sample: None,
        });

        process_string_tester(StringTesterParams {
            sample: Property::Uint("string_val".to_string(), 123),
            process_ok: false,
            previous_sample: None,
        });
    }

    fn convert_vector_to_int_histogram(hist: Vec<i64>) -> Property {
        let size = hist.len();
        Property::IntArray(
            "Bloop".to_string(),
            ArrayContent::LinearHistogram(LinearHistogram {
                floor: 1,
                step: 1,
                counts: hist,
                size,
                indexes: None,
            }),
        )
    }

    fn convert_vector_to_uint_histogram(hist: Vec<u64>) -> Property<String> {
        let size = hist.len();
        Property::UintArray(
            "Bloop".to_string(),
            ArrayContent::LinearHistogram(LinearHistogram {
                floor: 1,
                step: 1,
                counts: hist,
                size,
                indexes: None,
            }),
        )
    }

    // Produce condensed histograms. Size is arbitrary 100 - indexes must be less than that.
    fn convert_vectors_to_int_histogram(counts: Vec<i64>, indexes: Vec<usize>) -> Property<String> {
        let size = 100;
        Property::IntArray(
            "Bloop".to_string(),
            ArrayContent::LinearHistogram(LinearHistogram {
                floor: 1,
                step: 1,
                counts,
                size,
                indexes: Some(indexes),
            }),
        )
    }

    fn convert_vectors_to_uint_histogram(
        counts: Vec<u64>,
        indexes: Vec<usize>,
    ) -> Property<String> {
        let size = 100;
        Property::UintArray(
            "Bloop".to_string(),
            ArrayContent::LinearHistogram(LinearHistogram {
                floor: 1,
                step: 1,
                counts,
                size,
                indexes: Some(indexes),
            }),
        )
    }

    struct IntHistogramTesterParams {
        new_val: Property,
        old_val: Option<Property>,
        process_ok: bool,
        event_made: bool,
        diff: Vec<(u32, u64)>,
    }
    fn process_int_histogram_tester(params: IntHistogramTesterParams) {
        let data_source = MetricCacheKey {
            handle_name: InspectHandleName::name("foo.file"),
            selector: "test:root:count".to_string(),
        };
        let event_res =
            process_int_histogram(&params.new_val, params.old_val.as_ref(), &data_source);

        if !params.process_ok {
            assert!(event_res.is_err());
            return;
        }

        assert!(event_res.is_ok());

        let event_opt = event_res.unwrap();
        if !params.event_made {
            assert!(event_opt.is_none());
            return;
        }

        assert!(event_opt.is_some());

        let event = event_opt.unwrap();
        match event.clone() {
            MetricEventPayload::Histogram(histogram_buckets) => {
                assert_eq!(histogram_buckets.len(), params.diff.len());

                let expected_histogram_buckets = params
                    .diff
                    .iter()
                    .map(|(index, count)| HistogramBucket { index: *index, count: *count })
                    .collect::<Vec<HistogramBucket>>();

                assert_eq!(histogram_buckets, expected_histogram_buckets);
            }
            _ => panic!("Expecting int histogram."),
        }
    }

    /// Test that simple in-bounds first-samples of both types of Inspect histograms
    /// produce correct event types.
    #[fuchsia::test]
    fn test_normal_process_int_histogram() {
        let new_i64_sample = convert_vector_to_int_histogram(vec![1, 1, 1, 1]);
        let new_u64_sample = convert_vector_to_uint_histogram(vec![1, 1, 1, 1]);

        process_int_histogram_tester(IntHistogramTesterParams {
            new_val: new_i64_sample,
            old_val: None,
            process_ok: true,
            event_made: true,
            diff: vec![(0, 1), (1, 1), (2, 1), (3, 1)],
        });

        process_int_histogram_tester(IntHistogramTesterParams {
            new_val: new_u64_sample,
            old_val: None,
            process_ok: true,
            event_made: true,
            diff: vec![(0, 1), (1, 1), (2, 1), (3, 1)],
        });

        // Test an Inspect uint histogram at the boundaries of the type produce valid
        // cobalt events.
        let new_u64_sample = convert_vector_to_uint_histogram(vec![u64::MAX, u64::MAX, u64::MAX]);
        process_int_histogram_tester(IntHistogramTesterParams {
            new_val: new_u64_sample,
            old_val: None,
            process_ok: true,
            event_made: true,
            diff: vec![(0, u64::MAX), (1, u64::MAX), (2, u64::MAX)],
        });

        // Test that an empty Inspect histogram produces no event.
        let new_u64_sample = convert_vector_to_uint_histogram(Vec::new());
        process_int_histogram_tester(IntHistogramTesterParams {
            new_val: new_u64_sample,
            old_val: None,
            process_ok: true,
            event_made: false,
            diff: Vec::new(),
        });

        let new_u64_sample = convert_vector_to_uint_histogram(vec![0, 0, 0, 0]);
        process_int_histogram_tester(IntHistogramTesterParams {
            new_val: new_u64_sample,
            old_val: None,
            process_ok: true,
            event_made: false,
            diff: Vec::new(),
        });

        // Test that monotonically increasing histograms are good!.
        let new_u64_sample = convert_vector_to_uint_histogram(vec![2, 1, 2, 1]);
        let old_u64_sample = Some(convert_vector_to_uint_histogram(vec![1, 1, 0, 1]));
        process_int_histogram_tester(IntHistogramTesterParams {
            new_val: new_u64_sample,
            old_val: old_u64_sample,
            process_ok: true,
            event_made: true,
            diff: vec![(0, 1), (2, 2)],
        });

        let new_i64_sample = convert_vector_to_int_histogram(vec![5, 2, 1, 3]);
        let old_i64_sample = Some(convert_vector_to_int_histogram(vec![1, 1, 1, 1]));
        process_int_histogram_tester(IntHistogramTesterParams {
            new_val: new_i64_sample,
            old_val: old_i64_sample,
            process_ok: true,
            event_made: true,
            diff: vec![(0, 4), (1, 1), (3, 2)],
        });

        // Test that changing the histogram type resets the cache.
        let new_u64_sample = convert_vector_to_uint_histogram(vec![2, 1, 1, 1]);
        let old_i64_sample = Some(convert_vector_to_int_histogram(vec![1, 1, 1, 1]));
        process_int_histogram_tester(IntHistogramTesterParams {
            new_val: new_u64_sample,
            old_val: old_i64_sample,
            process_ok: true,
            event_made: true,
            diff: vec![(0, 2), (1, 1), (2, 1), (3, 1)],
        });
    }

    // Test that we can handle condensed int and uint histograms, even with indexes out of order
    #[fuchsia::test]
    fn test_normal_process_condensed_histograms() {
        let new_u64_sample = convert_vectors_to_int_histogram(vec![2, 6], vec![3, 5]);
        let old_u64_sample = Some(convert_vectors_to_int_histogram(vec![1], vec![5]));
        process_int_histogram_tester(IntHistogramTesterParams {
            new_val: new_u64_sample,
            old_val: old_u64_sample,
            process_ok: true,
            event_made: true,
            diff: vec![(3, 2), (5, 5)],
        });
        let new_i64_sample = convert_vectors_to_uint_histogram(vec![2, 4], vec![5, 3]);
        let old_i64_sample = None;
        process_int_histogram_tester(IntHistogramTesterParams {
            new_val: new_i64_sample,
            old_val: old_i64_sample,
            process_ok: true,
            event_made: true,
            diff: vec![(3, 4), (5, 2)],
        });
    }

    #[fuchsia::test]
    fn test_errorful_process_int_histogram() {
        // Test that changing the histogram length is an error.
        let new_u64_sample = convert_vector_to_uint_histogram(vec![1, 1, 1, 1]);
        let old_u64_sample = Some(convert_vector_to_uint_histogram(vec![1, 1, 1, 1, 1]));
        process_int_histogram_tester(IntHistogramTesterParams {
            new_val: new_u64_sample,
            old_val: old_u64_sample,
            process_ok: false,
            event_made: false,
            diff: Vec::new(),
        });

        // Test that new samples cant have negative values.
        let new_i64_sample = convert_vector_to_int_histogram(vec![1, 1, -1, 1]);
        process_int_histogram_tester(IntHistogramTesterParams {
            new_val: new_i64_sample,
            old_val: None,
            process_ok: false,
            event_made: false,
            diff: Vec::new(),
        });

        // Test that histograms must be monotonically increasing.
        let new_i64_sample = convert_vector_to_int_histogram(vec![5, 2, 1, 3]);
        let old_i64_sample = Some(convert_vector_to_int_histogram(vec![6, 1, 1, 1]));
        process_int_histogram_tester(IntHistogramTesterParams {
            new_val: new_i64_sample,
            old_val: old_i64_sample,
            process_ok: false,
            event_made: false,
            diff: Vec::new(),
        });
    }

    /// Ensure that data distinguished only by metadata handle name - with the same moniker and
    /// selector path - is kept properly separate in the previous-value cache. The same
    /// MetricConfig should match each data source, but the occurrence counts
    /// should reflect that the distinct values are individually tracked.
    #[fuchsia::test]
    async fn test_inspect_handle_name_distinguishes_data() {
        let mut sampler = ProjectSampler {
            archive_reader: ArchiveReader::new(),
            metrics: vec![],
            metric_cache: RefCell::new(HashMap::new()),
            metric_loggers: HashMap::new(),
            project_id: 1,
            poll_rate_sec: 3600,
            project_sampler_stats: Arc::new(ProjectSamplerStats::new()),
            all_done: true,
        };
        let selector: String = "my/component:[...]root/branch:leaf".to_string();
        let metric_id = 1;
        let event_codes = vec![];
        sampler.push_metric(MetricConfig {
            project_id: None,
            selectors: SelectorList::from(vec![sampler_config::parse_selector_for_test(&selector)]),
            metric_id,
            metric_type: DataType::Occurrence,
            event_codes,
            upload_once: Some(false),
        });
        sampler.rebuild_selector_data_structures();

        let data1_value4 = vec![InspectDataBuilder::new(
            "my/component".try_into().unwrap(),
            "component-url",
            Timestamp::from_nanos(0),
        )
        .with_hierarchy(hierarchy! { root: {branch: {leaf: 4i32}}})
        .with_name(InspectHandleName::name("name1"))
        .build()];
        let data2_value3 = vec![InspectDataBuilder::new(
            "my/component".try_into().unwrap(),
            "component-url",
            Timestamp::from_nanos(0),
        )
        .with_hierarchy(hierarchy! { root: {branch: {leaf: 3i32}}})
        .with_name(InspectHandleName::name("name2"))
        .build()];
        let data1_value6 = vec![InspectDataBuilder::new(
            "my/component".try_into().unwrap(),
            "component-url",
            Timestamp::from_nanos(0),
        )
        .with_hierarchy(hierarchy! { root: {branch: {leaf: 6i32}}})
        .with_name(InspectHandleName::name("name1"))
        .build()];
        let data2_value8 = vec![InspectDataBuilder::new(
            "my/component".try_into().unwrap(),
            "component-url",
            Timestamp::from_nanos(0),
        )
        .with_hierarchy(hierarchy! { root: {branch: {leaf: 8i32}}})
        .with_name(InspectHandleName::name("name2"))
        .build()];

        fn expect_one_metric_event_value(
            events: Result<Vec<EventToLog>, Error>,
            value: u64,
            context: &'static str,
        ) {
            let events = events.expect(context);
            assert_eq!(events.len(), 1, "Events len not 1: {}: {}", context, events.len());
            let event = &events[0];
            let (project_id, MetricEvent { payload, .. }) = event;
            assert_eq!(*project_id, 1);
            if let fidl_fuchsia_metrics::MetricEventPayload::Count(payload) = payload {
                assert_eq!(
                    payload, &value,
                    "Wrong payload, expected {} got {} at {}",
                    value, payload, context
                );
            } else {
                panic!("Expected MetricEventPayload::Count at {}, got {:?}", context, payload);
            }
        }

        expect_one_metric_event_value(sampler.process_snapshot(data1_value4).await, 4, "first");
        expect_one_metric_event_value(sampler.process_snapshot(data2_value3).await, 3, "second");
        expect_one_metric_event_value(sampler.process_snapshot(data1_value6).await, 2, "third");
        expect_one_metric_event_value(sampler.process_snapshot(data2_value8).await, 5, "fourth");
    }

    // TODO(https://fxbug.dev/42071858): we should remove this once we support batching.
    #[fuchsia::test]
    async fn project_id_can_be_overwritten_by_the_metric_project_id() {
        let mut sampler = ProjectSampler {
            archive_reader: ArchiveReader::new(),
            metrics: vec![],
            metric_cache: RefCell::new(HashMap::new()),
            metric_loggers: HashMap::new(),
            project_id: 1,
            poll_rate_sec: 3600,
            project_sampler_stats: Arc::new(ProjectSamplerStats::new()),
            all_done: true,
        };
        let selector: String = "my/component:[name=name1]root/branch:leaf".to_string();
        let metric_id = 1;
        let event_codes = vec![];
        sampler.push_metric(MetricConfig {
            project_id: Some(2),
            selectors: SelectorList::from(vec![sampler_config::parse_selector_for_test(&selector)]),
            metric_id,
            metric_type: DataType::Occurrence,
            event_codes,
            upload_once: Some(false),
        });
        sampler.rebuild_selector_data_structures();

        let value = vec![InspectDataBuilder::new(
            "my/component".try_into().unwrap(),
            "component-url",
            Timestamp::from_nanos(0),
        )
        .with_hierarchy(hierarchy! { root: {branch: {leaf: 4i32}}})
        .with_name(InspectHandleName::name("name1"))
        .build()];

        let events = sampler.process_snapshot(value).await.expect("processed snapshot");
        assert_eq!(events.len(), 1);
        let event = &events[0];
        let (project_id, MetricEvent { .. }) = event;
        assert_eq!(*project_id, 2);
    }
}
