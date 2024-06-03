// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    events::{
        router::EventConsumer,
        types::{Event, EventPayload, LogSinkRequestedPayload},
    },
    identity::ComponentIdentity,
    logs::{
        budget::BudgetManager,
        container::LogsArtifactsContainer,
        debuglog::{DebugLog, DebugLogBridge, KERNEL_IDENTITY},
        multiplex::{Multiplexer, MultiplexerHandle},
        stored_message::StoredMessage,
    },
    severity_filter::KlogSeverityFilter,
};
use diagnostics_data::LogsData;
use fidl_fuchsia_diagnostics::{LogInterestSelector, Selector, Severity, StreamMode};
use fuchsia_async as fasync;
use fuchsia_inspect as inspect;
use fuchsia_sync::{Mutex, RwLock};
use fuchsia_trace as ftrace;
use futures::channel::mpsc;
use futures::prelude::*;
use lazy_static::lazy_static;
use moniker::Moniker;
use selectors::SelectorExt;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tracing::{debug, error};

lazy_static! {
    pub static ref INTEREST_CONNECTION_ID: AtomicUsize = AtomicUsize::new(0);
    pub static ref OUR_MONIKER: Moniker = Moniker::parse_str("bootstrap/archivist").unwrap();
}

/// LogsRepository holds all diagnostics data and is a singleton wrapped by multiple
/// [`pipeline::Pipeline`]s in a given Archivist instance.
pub struct LogsRepository {
    log_sender: RwLock<mpsc::UnboundedSender<fasync::Task<()>>>,
    mutable_state: RwLock<LogsRepositoryState>,
    /// Processes removal of components emitted by the budget handler. This is done to prevent a
    /// deadlock. It's behind a mutex, but it's only set once. Holds an Arc<Self>.
    component_removal_task: Mutex<Option<fasync::Task<()>>>,
}

impl LogsRepository {
    pub fn new(logs_max_cached_original_bytes: u64, parent: &fuchsia_inspect::Node) -> Arc<Self> {
        let (remover_snd, remover_rcv) = mpsc::unbounded();
        let logs_budget = BudgetManager::new(logs_max_cached_original_bytes as usize, remover_snd);
        let (log_sender, log_receiver) = mpsc::unbounded();
        let this = Arc::new(LogsRepository {
            mutable_state: RwLock::new(LogsRepositoryState::new(logs_budget, log_receiver, parent)),
            log_sender: RwLock::new(log_sender),
            component_removal_task: Mutex::new(None),
        });
        *this.component_removal_task.lock() = Some(fasync::Task::spawn(
            Self::process_removal_of_components(remover_rcv, Arc::clone(&this)),
        ));
        this
    }

    /// Drain the kernel's debug log. The returned future completes once
    /// existing messages have been ingested.
    pub fn drain_debuglog<K>(&self, klog_reader: K)
    where
        K: DebugLog + Send + Sync + 'static,
    {
        let mut mutable_state = self.mutable_state.write();
        // We can only have one klog reader, if this is already set, it means we are already
        // draining klog.
        if mutable_state.drain_klog_task.is_some() {
            return;
        }

        let container = mutable_state.get_log_container(KERNEL_IDENTITY.clone());
        mutable_state.drain_klog_task = Some(fasync::Task::spawn(async move {
            debug!("Draining debuglog.");
            let mut kernel_logger =
                DebugLogBridge::create(klog_reader, Arc::clone(&container.stats));
            let mut messages = match kernel_logger.existing_logs() {
                Ok(messages) => messages,
                Err(e) => {
                    error!(%e, "failed to read from kernel log, important logs may be missing");
                    return;
                }
            };
            messages.sort_by_key(|m| m.timestamp());
            for message in messages {
                container.ingest_message(Box::new(message));
            }

            let res = kernel_logger
                .listen()
                .try_for_each(|message| async {
                    container.ingest_message(Box::new(message));
                    Ok(())
                })
                .await;
            if let Err(e) = res {
                error!(%e, "failed to drain kernel log, important logs may be missing");
            }
        }));
    }

    pub fn logs_cursor(
        &self,
        mode: StreamMode,
        selectors: Option<Vec<Selector>>,
        parent_trace_id: ftrace::Id,
    ) -> impl Stream<Item = Arc<LogsData>> + Send + 'static {
        let mut repo = self.mutable_state.write();
        let substreams = repo.logs_data_store.iter().map(|(identity, c)| {
            let cursor = c.cursor(mode, parent_trace_id);
            (Arc::clone(identity), cursor)
        });
        let (mut merged, mpx_handle) = Multiplexer::new(parent_trace_id, selectors, substreams);
        repo.logs_multiplexers.add(mode, mpx_handle);
        merged.set_on_drop_id_sender(repo.logs_multiplexers.cleanup_sender());
        merged
    }

    pub fn get_log_container(
        &self,
        identity: Arc<ComponentIdentity>,
    ) -> Arc<LogsArtifactsContainer> {
        self.mutable_state.write().get_log_container(identity)
    }

    /// Stop accepting new messages, ensuring that pending Cursors return Poll::Ready(None) after
    /// consuming any messages received before this call.
    pub async fn wait_for_termination(&self) {
        let receiver = self.mutable_state.write().log_receiver.take().unwrap();
        receiver.for_each_concurrent(None, |rx| rx).await;
        // Process messages from log sink.
        debug!("Log ingestion stopped.");
        let mut repo = self.mutable_state.write();
        for container in repo.logs_data_store.values() {
            container.terminate();
        }
        repo.logs_multiplexers.terminate();

        debug!("Terminated logs");
        repo.logs_budget.terminate();
    }

    /// Closes the connection in which new logger draining tasks are sent. No more logger tasks
    /// will be accepted when this is called and we'll proceed to terminate logs.
    pub fn stop_accepting_new_log_sinks(&self) {
        self.log_sender.write().disconnect();
    }

    /// Returns an id to use for a new interest connection. Used by both LogSettings and Log, to
    /// ensure shared uniqueness of their connections.
    pub fn new_interest_connection(&self) -> usize {
        INTEREST_CONNECTION_ID.fetch_add(1, Ordering::Relaxed)
    }

    /// Updates log selectors associated with an interest connection.
    pub fn update_logs_interest(&self, connection_id: usize, selectors: Vec<LogInterestSelector>) {
        self.mutable_state.write().update_logs_interest(connection_id, selectors);
    }

    /// Indicates that the connection associated with the given ID is now done.
    pub fn finish_interest_connection(&self, connection_id: usize) {
        self.mutable_state.write().finish_interest_connection(connection_id);
    }

    async fn process_removal_of_components(
        mut removal_requests: mpsc::UnboundedReceiver<Arc<ComponentIdentity>>,
        logs_repo: Arc<LogsRepository>,
    ) {
        while let Some(identity) = removal_requests.next().await {
            let mut repo = logs_repo.mutable_state.write();
            if !repo.is_live(&identity) {
                debug!(%identity, "Removing component from repository.");
                repo.remove(&identity);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn default() -> Arc<Self> {
        LogsRepository::new(
            crate::constants::LEGACY_DEFAULT_MAXIMUM_CACHED_LOGS_BYTES,
            &Default::default(),
        )
    }
}

impl EventConsumer for LogsRepository {
    fn handle(self: Arc<Self>, event: Event) {
        match event.payload {
            EventPayload::LogSinkRequested(LogSinkRequestedPayload {
                component,
                request_stream,
            }) => {
                debug!(identity = %component, "LogSink requested.");
                let container = self.get_log_container(component);
                container.handle_log_sink(request_stream, self.log_sender.read().clone());
            }
            _ => unreachable!("Archivist state just subscribes to log sink requested"),
        }
    }
}

pub struct LogsRepositoryState {
    logs_data_store: HashMap<Arc<ComponentIdentity>, Arc<LogsArtifactsContainer>>,
    inspect_node: inspect::Node,

    /// Receives the logger tasks. This will be taken once in wait for termination hence why it's
    /// an option.
    log_receiver: Option<mpsc::UnboundedReceiver<fasync::Task<()>>>,

    /// A reference to the budget manager, kept to be passed to containers.
    logs_budget: BudgetManager,
    /// BatchIterators for logs need to be made aware of new components starting and their logs.
    logs_multiplexers: MultiplexerBroker,

    /// Interest registrations that we have received through fuchsia.logger.Log/ListWithSelectors
    /// or through fuchsia.logger.LogSettings/SetInterest.
    interest_registrations: BTreeMap<usize, Vec<LogInterestSelector>>,

    /// The task draining klog and routing to syslog.
    drain_klog_task: Option<fasync::Task<()>>,
}

impl LogsRepositoryState {
    fn new(
        logs_budget: BudgetManager,
        log_receiver: mpsc::UnboundedReceiver<fasync::Task<()>>,
        parent: &fuchsia_inspect::Node,
    ) -> Self {
        Self {
            inspect_node: parent.create_child("log_sources"),
            logs_data_store: HashMap::new(),
            logs_budget,
            log_receiver: Some(log_receiver),
            logs_multiplexers: MultiplexerBroker::new(),
            interest_registrations: BTreeMap::new(),
            drain_klog_task: None,
        }
    }

    /// Returns a container for logs artifacts, constructing one and adding it to the trie if
    /// necessary.
    pub fn get_log_container(
        &mut self,
        identity: Arc<ComponentIdentity>,
    ) -> Arc<LogsArtifactsContainer> {
        match self.logs_data_store.get(&identity) {
            None => {
                let container = Arc::new(LogsArtifactsContainer::new(
                    Arc::clone(&identity),
                    self.interest_registrations.values().flat_map(|s| s.iter()),
                    &self.inspect_node,
                    self.logs_budget.handle(),
                ));
                self.logs_budget.add_container(Arc::clone(&container));
                self.logs_data_store.insert(identity, Arc::clone(&container));
                self.logs_multiplexers.send(&container);
                container
            }
            Some(existing) => Arc::clone(existing),
        }
    }

    fn is_live(&self, identity: &Arc<ComponentIdentity>) -> bool {
        match self.logs_data_store.get(identity) {
            Some(container) => container.should_retain(),
            None => false,
        }
    }

    /// Updates our own log interest if we are the root Archivist and logging
    /// to klog.
    fn maybe_update_own_logs_interest(
        &mut self,
        selectors: &[LogInterestSelector],
        clear_interest: bool,
    ) {
        tracing::dispatcher::get_default(|dispatcher| {
            let Some(publisher) = dispatcher.downcast_ref::<KlogSeverityFilter>() else {
                return;
            };
            let lowest_selector = selectors
                .iter()
                .filter(|selector| {
                    OUR_MONIKER.matches_component_selector(&selector.selector).unwrap_or(false)
                })
                .min_by_key(|selector| selector.interest.min_severity.unwrap_or(Severity::Info));
            if let Some(selector) = lowest_selector {
                if clear_interest {
                    *publisher.min_severity.write() = Severity::Info;
                } else {
                    *publisher.min_severity.write() =
                        selector.interest.min_severity.unwrap_or(Severity::Info);
                }
            }
        });
    }

    fn update_logs_interest(&mut self, connection_id: usize, selectors: Vec<LogInterestSelector>) {
        self.maybe_update_own_logs_interest(&selectors, false);
        let previous_selectors =
            self.interest_registrations.insert(connection_id, selectors).unwrap_or_default();
        // unwrap safe, we just inserted.
        let new_selectors = self.interest_registrations.get(&connection_id).unwrap();
        for logs_data in self.logs_data_store.values() {
            logs_data.update_interest(new_selectors.iter(), &previous_selectors);
        }
    }

    pub fn finish_interest_connection(&mut self, connection_id: usize) {
        let selectors = self.interest_registrations.remove(&connection_id);
        if let Some(selectors) = selectors {
            self.maybe_update_own_logs_interest(&selectors, true);
            for logs_data in self.logs_data_store.values() {
                logs_data.reset_interest(&selectors);
            }
        }
    }

    pub fn remove(&mut self, identity: &Arc<ComponentIdentity>) {
        self.logs_data_store.remove(identity);
    }
}

type LiveIteratorsMap = HashMap<usize, (StreamMode, MultiplexerHandle<Arc<LogsData>>)>;

/// Ensures that BatchIterators get access to logs from newly started components.
pub struct MultiplexerBroker {
    live_iterators: Arc<Mutex<LiveIteratorsMap>>,
    cleanup_sender: mpsc::UnboundedSender<usize>,
    _live_iterators_cleanup_task: fasync::Task<()>,
}

impl MultiplexerBroker {
    fn new() -> Self {
        let (cleanup_sender, mut receiver) = mpsc::unbounded();
        let live_iterators = Arc::new(Mutex::new(HashMap::new()));
        let live_iterators_clone = Arc::clone(&live_iterators);
        Self {
            live_iterators,
            cleanup_sender,
            _live_iterators_cleanup_task: fasync::Task::spawn(async move {
                while let Some(id) = receiver.next().await {
                    live_iterators_clone.lock().remove(&id);
                }
            }),
        }
    }

    fn cleanup_sender(&self) -> mpsc::UnboundedSender<usize> {
        self.cleanup_sender.clone()
    }

    /// A new BatchIterator has been created and must be notified when future log containers are
    /// created.
    fn add(&mut self, mode: StreamMode, recipient: MultiplexerHandle<Arc<LogsData>>) {
        match mode {
            // snapshot streams only want to know about what's currently available
            StreamMode::Snapshot => recipient.close(),
            StreamMode::SnapshotThenSubscribe | StreamMode::Subscribe => {
                self.live_iterators.lock().insert(recipient.multiplexer_id(), (mode, recipient));
            }
        }
    }

    /// Notify existing BatchIterators of a new logs container so they can include its messages
    /// in their results.
    pub fn send(&mut self, container: &Arc<LogsArtifactsContainer>) {
        self.live_iterators.lock().retain(|_, (mode, recipient)| {
            recipient.send(
                Arc::clone(&container.identity),
                container.cursor(*mode, recipient.parent_trace_id()),
            )
        });
    }

    /// Notify all multiplexers to terminate their streams once sub streams have terminated.
    fn terminate(&mut self) {
        for (_, (_, recipient)) in self.live_iterators.lock().drain() {
            recipient.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::logs::stored_message::{GenericStoredMessage, StructuredStoredMessage},
        diagnostics_log_encoding::{
            encode::Encoder, Argument, Record, Severity as StreamSeverity, Value,
        },
        moniker::ExtendedMoniker,
        selectors::FastError,
        std::{io::Cursor, time::Duration},
    };

    #[fuchsia::test]
    async fn data_repo_filters_logs_by_selectors() {
        let repo = LogsRepository::default();
        let foo_container = repo.get_log_container(Arc::new(ComponentIdentity::new(
            ExtendedMoniker::parse_str("./foo").unwrap(),
            "fuchsia-pkg://foo",
        )));
        let bar_container = repo.get_log_container(Arc::new(ComponentIdentity::new(
            ExtendedMoniker::parse_str("./bar").unwrap(),
            "fuchsia-pkg://bar",
        )));

        foo_container.ingest_message(make_message("a", 1));
        bar_container.ingest_message(make_message("b", 2));
        foo_container.ingest_message(make_message("c", 3));

        let stream = repo.logs_cursor(StreamMode::Snapshot, None, ftrace::Id::random());

        let results =
            stream.map(|value| value.msg().unwrap().to_string()).collect::<Vec<_>>().await;
        assert_eq!(results, vec!["a".to_string(), "b".to_string(), "c".to_string()]);

        let filtered_stream = repo.logs_cursor(
            StreamMode::Snapshot,
            Some(vec![selectors::parse_selector::<FastError>("foo:root").unwrap()]),
            ftrace::Id::random(),
        );

        let results =
            filtered_stream.map(|value| value.msg().unwrap().to_string()).collect::<Vec<_>>().await;
        assert_eq!(results, vec!["a".to_string(), "c".to_string()]);
    }

    #[fuchsia::test]
    async fn multiplexer_broker_cleanup() {
        let repo = LogsRepository::default();
        let stream =
            repo.logs_cursor(StreamMode::SnapshotThenSubscribe, None, ftrace::Id::random());

        assert_eq!(repo.mutable_state.read().logs_multiplexers.live_iterators.lock().len(), 1);

        // When the multiplexer goes away it must be forgotten by the broker.
        drop(stream);
        loop {
            fasync::Timer::new(Duration::from_millis(100)).await;
            if repo.mutable_state.read().logs_multiplexers.live_iterators.lock().len() == 0 {
                break;
            }
        }
    }

    fn make_message(msg: &str, timestamp: i64) -> GenericStoredMessage {
        let record = Record {
            timestamp,
            severity: StreamSeverity::Debug.into_primitive(),
            arguments: vec![
                Argument { name: "pid".to_string(), value: Value::UnsignedInt(1) },
                Argument { name: "tid".to_string(), value: Value::UnsignedInt(2) },
                Argument { name: "message".to_string(), value: Value::Text(msg.to_string()) },
            ],
        };
        let mut buffer = Cursor::new(vec![0u8; 1024]);
        let mut encoder = Encoder::new(&mut buffer);
        encoder.write_record(&record).unwrap();
        let encoded = &buffer.get_ref()[..buffer.position() as usize];
        StructuredStoredMessage::create(encoded.to_vec(), Default::default())
    }
}
