// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::diagnostics::TRACE_CATEGORY;
use crate::identity::ComponentIdentity;
use crate::logs::multiplex::PinStream;
use crate::logs::shared_buffer::{ContainerBuffer, LazyItem};
use crate::logs::socket::{Encoding, LogMessageSocket};
use crate::logs::stats::LogStreamStats;
use crate::logs::stored_message::StoredMessage;
use crate::utils::AutoCall;
use derivative::Derivative;
use diagnostics_data::{BuilderArgs, Data, LogError, Logs, LogsData, LogsDataBuilder};
use fidl_fuchsia_diagnostics::{
    Interest as FidlInterest, LogInterestSelector, Severity as FidlSeverity, StreamMode,
};
use fidl_fuchsia_logger::{
    InterestChangeError, LogSinkRequest, LogSinkRequestStream,
    LogSinkWaitForInterestChangeResponder,
};
use fuchsia_async::Task;
use fuchsia_sync::Mutex;
use futures::channel::{mpsc, oneshot};
use futures::prelude::*;
use selectors::SelectorExt;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tracing::{debug, error, warn};
use {fuchsia_async as fasync, fuchsia_trace as ftrace};

pub type OnInactive = Box<dyn Fn(&LogsArtifactsContainer) + Send + Sync>;

#[derive(Derivative)]
#[derivative(Debug)]
pub struct LogsArtifactsContainer {
    /// The source of logs in this container.
    pub identity: Arc<ComponentIdentity>,

    /// Inspect instrumentation.
    pub stats: Arc<LogStreamStats>,

    /// Buffer for all log messages.
    #[derivative(Debug = "ignore")]
    buffer: ContainerBuffer,

    /// Mutable state for the container.
    state: Arc<Mutex<ContainerState>>,

    /// Current object ID used in place of a memory address
    /// used to uniquely identify an object in a BTreeMap.
    next_hanging_get_id: AtomicUsize,

    /// Mechanism for a test to retrieve the internal hanging get state.
    hanging_get_test_state: Arc<Mutex<TestState>>,

    /// A callback which is called when the container is inactive i.e. has no channels, sockets or
    /// stored logs.
    #[derivative(Debug = "ignore")]
    on_inactive: Option<OnInactive>,
}

#[derive(PartialEq, Debug)]
enum TestState {
    /// Blocked -- waiting for interest change
    Blocked,
    /// No FIDL request received yet
    NoRequest,
}

type InterestSender = oneshot::Sender<Result<FidlInterest, InterestChangeError>>;

#[derive(Debug)]
struct ContainerState {
    /// Number of sockets currently being drained for this component.
    num_active_sockets: u64,

    /// Number of LogSink channels currently being listened to for this component.
    num_active_channels: u64,

    /// Current interest for this component.
    interests: BTreeMap<Interest, usize>,

    /// Hanging gets
    hanging_gets: BTreeMap<usize, Arc<Mutex<Option<InterestSender>>>>,

    is_initializing: bool,
}

impl LogsArtifactsContainer {
    pub fn new<'a>(
        identity: Arc<ComponentIdentity>,
        interest_selectors: impl Iterator<Item = &'a LogInterestSelector>,
        initial_interest: Option<FidlSeverity>,
        stats: Arc<LogStreamStats>,
        buffer: ContainerBuffer,
        on_inactive: Option<OnInactive>,
    ) -> Self {
        let mut interests = BTreeMap::new();
        if let Some(severity) = initial_interest {
            interests.insert(Interest::from(severity), 1);
        }
        let new = Self {
            identity,
            buffer,
            state: Arc::new(Mutex::new(ContainerState {
                num_active_channels: 0,
                num_active_sockets: 0,
                interests,
                hanging_gets: BTreeMap::new(),
                is_initializing: true,
            })),
            stats,
            next_hanging_get_id: AtomicUsize::new(0),
            hanging_get_test_state: Arc::new(Mutex::new(TestState::NoRequest)),
            on_inactive,
        };

        // there are no control handles so this won't notify anyone
        new.update_interest(interest_selectors, &[]);

        new
    }

    fn fetch_add_hanging_get_id(&self) -> usize {
        self.next_hanging_get_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Returns a stream of this component's log messages.
    ///
    /// # Rolled out logs
    ///
    /// When messages are evicted from our internal buffers before a client can read them, they
    /// are counted as rolled out messages which gets appended to the metadata of the next message.
    /// If there is no next message, there is no way to know how many messages were rolled out.
    pub fn cursor(
        &self,
        mode: StreamMode,
        parent_trace_id: ftrace::Id,
    ) -> PinStream<Arc<LogsData>> {
        let Some(buffer_cursor) = self.buffer.cursor(mode) else {
            return Box::pin(futures::stream::empty());
        };

        let identity = Arc::clone(&self.identity);
        Box::pin(
            buffer_cursor
                .enumerate()
                .scan(
                    (zx::BootInstant::ZERO, 0u64),
                    move |(last_timestamp, rolled_out_messages), (i, item)| {
                        futures::future::ready(match item {
                            LazyItem::Next(m) => {
                                let trace_id = ftrace::Id::random();
                                let _trace_guard = ftrace::async_enter!(
                                    trace_id,
                                    TRACE_CATEGORY,
                                    c"LogContainer::cursor.parse_message",
                                    // An async duration cannot have multiple concurrent child async durations
                                    // so we include the nonce as metadata to manually determine relationship.
                                    "parent_trace_id" => u64::from(parent_trace_id),
                                    "trace_id" => u64::from(trace_id)
                                );
                                *last_timestamp = m.timestamp();
                                match m.parse(&identity) {
                                    Ok(m) => Some(Some(Arc::new(maybe_add_rolled_out_error(
                                        rolled_out_messages,
                                        m,
                                    )))),
                                    Err(err) => {
                                        let data = maybe_add_rolled_out_error(
                                            rolled_out_messages,
                                            LogsDataBuilder::new(BuilderArgs {
                                                moniker: identity.moniker.clone(),
                                                timestamp: *last_timestamp,
                                                component_url: Some(identity.url.clone()),
                                                severity: diagnostics_data::Severity::Warn,
                                            })
                                            .add_error(
                                                diagnostics_data::LogError::FailedToParseRecord(
                                                    format!("{err:?}"),
                                                ),
                                            )
                                            .build(),
                                        );
                                        Some(Some(Arc::new(data)))
                                    }
                                }
                            }
                            LazyItem::ItemsRolledOut(rolled_out_count, timestamp) => {
                                if i > 0 {
                                    *rolled_out_messages += rolled_out_count;
                                }
                                *last_timestamp = timestamp;
                                Some(None)
                            }
                        })
                    },
                )
                .filter_map(future::ready),
        )
    }

    /// Handle `LogSink` protocol on `stream`. Each socket received from the `LogSink` client is
    /// drained by a `Task` which is sent on `sender`. The `Task`s do not complete until their
    /// sockets have been closed.
    pub fn handle_log_sink(
        self: &Arc<Self>,
        stream: LogSinkRequestStream,
        sender: mpsc::UnboundedSender<Task<()>>,
    ) {
        {
            let mut guard = self.state.lock();
            guard.num_active_channels += 1;
            guard.is_initializing = false;
        }
        let task = Task::spawn(Arc::clone(self).actually_handle_log_sink(stream, sender.clone()));
        sender.unbounded_send(task).expect("channel is live for whole program");
    }

    /// This function does not return until the channel is closed.
    async fn actually_handle_log_sink(
        self: Arc<Self>,
        mut stream: LogSinkRequestStream,
        sender: mpsc::UnboundedSender<Task<()>>,
    ) {
        let hanging_get_sender = Arc::new(Mutex::new(None));

        let mut interest_listener = None;
        let previous_interest_sent = Arc::new(Mutex::new(None));
        debug!(%self.identity, "Draining LogSink channel.");

        macro_rules! handle_socket {
            ($ctor:ident($socket:ident)) => {{
                let socket = fasync::Socket::from_socket($socket);
                let log_stream = LogMessageSocket::$ctor(socket, self.stats.clone());
                self.state.lock().num_active_sockets += 1;
                let task = Task::spawn(self.clone().drain_messages(log_stream));
                sender.unbounded_send(task).expect("channel alive for whole program");
            }};
        }

        while let Some(next) = stream.next().await {
            match next {
                Ok(LogSinkRequest::Connect { socket, .. }) => {
                    handle_socket! {new(socket)};
                }
                Ok(LogSinkRequest::ConnectStructured { socket, .. }) => {
                    handle_socket! {new_structured(socket)};
                }
                Ok(LogSinkRequest::WaitForInterestChange { responder }) => {
                    // Check if we sent latest data to the client
                    let min_interest;
                    let needs_interest_broadcast;
                    {
                        let state = self.state.lock();
                        let previous_interest = previous_interest_sent.lock();
                        needs_interest_broadcast = {
                            if let Some(prev) = &*previous_interest {
                                *prev != state.min_interest()
                            } else {
                                true
                            }
                        };
                        min_interest = state.min_interest();
                    }
                    if needs_interest_broadcast {
                        // Send interest if not yet received
                        let _ = responder.send(Ok(&min_interest));
                        let mut previous_interest = previous_interest_sent.lock();
                        *previous_interest = Some(min_interest);
                    } else {
                        // Wait for broadcast event asynchronously
                        self.wait_for_interest_change_async(
                            Arc::clone(&previous_interest_sent),
                            &mut interest_listener,
                            responder,
                            Arc::clone(&hanging_get_sender),
                        )
                        .await;
                    }
                }
                Err(e) => error!(%self.identity, %e, "error handling log sink"),
            }
        }
        debug!(%self.identity, "LogSink channel closed.");
        self.state.lock().num_active_channels -= 1;
        self.check_inactive();
    }

    async fn wait_for_interest_change_async(
        self: &Arc<Self>,
        previous_interest_sent: Arc<Mutex<Option<FidlInterest>>>,
        interest_listener: &mut Option<Task<()>>,
        responder: LogSinkWaitForInterestChangeResponder,
        sender: Arc<Mutex<Option<InterestSender>>>,
    ) {
        let (tx, rx) = oneshot::channel();
        {
            let mut locked_sender = sender.lock();
            if let Some(value) = locked_sender.take() {
                // Error to call API twice without waiting for first return
                let _ = value.send(Err(InterestChangeError::CalledTwice));
            }
            *locked_sender = Some(tx);
        }
        if let Some(listener) = interest_listener.take() {
            listener.await;
        }

        let mut state = self.state.lock();
        let id = self.fetch_add_hanging_get_id();
        {
            state.hanging_gets.insert(id, Arc::clone(&sender));
        }
        let unlocked_state = Arc::clone(&self.state);
        let prev_interest_clone = Arc::clone(&previous_interest_sent);
        let get_clone = Arc::clone(&self.hanging_get_test_state);
        *interest_listener = Some(Task::spawn(async move {
            // Block started
            if cfg!(test) {
                let mut get_state = get_clone.lock();
                *get_state = TestState::Blocked;
            }
            let _ac = AutoCall::new(|| {
                Task::spawn(async move {
                    let mut state = unlocked_state.lock();
                    state.hanging_gets.remove(&id);
                })
                .detach();
            });
            let res = rx.await;
            if let Ok(value) = res {
                match value {
                    Ok(value) => {
                        let _ = responder.send(Ok(&value));
                        let mut write_lock = prev_interest_clone.lock();
                        *write_lock = Some(value);
                    }
                    Err(error) => {
                        let _ = responder.send(Err(error));
                    }
                }
            }
            // No longer blocked
            if cfg!(test) {
                let mut get_state = get_clone.lock();
                *get_state = TestState::NoRequest;
            }
        }));
    }

    /// Drain a `LogMessageSocket` which wraps a socket from a component
    /// generating logs.
    pub async fn drain_messages<E>(self: Arc<Self>, mut log_stream: LogMessageSocket<E>)
    where
        E: Encoding + Unpin,
    {
        debug!(%self.identity, "Draining messages from a socket.");
        loop {
            match log_stream.next().await {
                Some(Ok(message)) => self.ingest_message(message),
                Some(Err(err)) => {
                    warn!(source = %self.identity, %err, "closing socket");
                    break;
                }
                None => break,
            }
        }
        debug!(%self.identity, "Socket closed.");
        self.state.lock().num_active_sockets -= 1;
        self.check_inactive();
    }

    /// Updates log stats in inspect and push the message onto the container's buffer.
    pub fn ingest_message(&self, message: StoredMessage) {
        self.stats.ingest_message(&message);
        self.buffer.push_back(message.bytes());
    }

    /// Set the `Interest` for this component, notifying all active `LogSink/WaitForInterestChange`
    /// hanging gets with the new interset if it is a change from the previous interest.
    /// For any match that is also contained in `previous_selectors`, the previous values will be
    /// removed from the set of interests.
    pub fn update_interest<'a>(
        &self,
        interest_selectors: impl Iterator<Item = &'a LogInterestSelector>,
        previous_selectors: &[LogInterestSelector],
    ) {
        let mut new_interest = FidlInterest::default();
        let mut remove_interest = FidlInterest::default();
        for selector in interest_selectors {
            if self
                .identity
                .moniker
                .matches_component_selector(&selector.selector)
                .unwrap_or_default()
            {
                new_interest = selector.interest.clone();
                // If there are more matches, ignore them, we'll pick the first match.
                break;
            }
        }

        if let Some(previous_selector) = previous_selectors.iter().find(|s| {
            self.identity.moniker.matches_component_selector(&s.selector).unwrap_or_default()
        }) {
            remove_interest = previous_selector.interest.clone();
        }

        let mut state = self.state.lock();
        // Unfortunately we cannot use a match statement since `FidlInterest` doesn't derive Eq.
        // It does derive PartialEq though. All these branches will send an interest update if the
        // minimum interest changes after performing the required actions.
        if new_interest == FidlInterest::default() && remove_interest != FidlInterest::default() {
            // Undo the previous interest. There's no new interest to add.
            state.maybe_send_updates(
                |state| {
                    state.erase(&remove_interest);
                },
                &self.identity,
            );
        } else if new_interest != FidlInterest::default()
            && remove_interest == FidlInterest::default()
        {
            // Apply the new interest. There's no previous interest to remove.
            state.maybe_send_updates(
                |state| {
                    state.push_interest(new_interest);
                },
                &self.identity,
            );
        } else if new_interest != FidlInterest::default()
            && remove_interest != FidlInterest::default()
        {
            // Remove the previous interest and insert the new one.
            state.maybe_send_updates(
                |state| {
                    state.erase(&remove_interest);
                    state.push_interest(new_interest);
                },
                &self.identity,
            );
        }
    }

    /// Resets the `Interest` for this component, notifying all active
    /// `LogSink/WaitForInterestChange` hanging gets with the lowest interest found in the set of
    /// requested interests for all control handles.
    pub fn reset_interest(&self, interest_selectors: &[LogInterestSelector]) {
        for selector in interest_selectors {
            if self
                .identity
                .moniker
                .matches_component_selector(&selector.selector)
                .unwrap_or_default()
            {
                self.state.lock().maybe_send_updates(
                    |state| {
                        state.erase(&selector.interest);
                    },
                    &self.identity,
                );
                return;
            }
        }
    }

    /// Returns `true` if this container corresponds to a running component, or still has pending
    /// objects to drain.
    pub fn is_active(&self) -> bool {
        let state = self.state.lock();
        state.is_initializing
            || state.num_active_sockets > 0
            || state.num_active_channels > 0
            || !self.buffer.is_empty()
    }

    /// Called whenever there's a transition that means the component might no longer be active.
    fn check_inactive(&self) {
        if !self.is_active() {
            if let Some(on_inactive) = &self.on_inactive {
                on_inactive(self);
            }
        }
    }

    /// Stop accepting new messages, ensuring that pending Cursors return Poll::Ready(None) after
    /// consuming any messages received before this call.
    pub fn terminate(&self) {
        self.buffer.terminate();
    }

    #[cfg(test)]
    pub fn mark_stopped(&self) {
        self.state.lock().is_initializing = false;
        self.check_inactive();
    }
}

fn maybe_add_rolled_out_error(rolled_out_messages: &mut u64, mut msg: Data<Logs>) -> Data<Logs> {
    if *rolled_out_messages != 0 {
        // Add rolled out metadata
        msg.metadata
            .errors
            .get_or_insert(vec![])
            .push(LogError::RolledOutLogs { count: *rolled_out_messages });
    }
    *rolled_out_messages = 0;
    msg
}

impl ContainerState {
    /// Executes the given callback on the state. If the minimum interest before executing the given
    /// actions and after isn't the same, then the new interest is sent to the registered listeners.
    fn maybe_send_updates<F>(&mut self, action: F, identity: &ComponentIdentity)
    where
        F: FnOnce(&mut ContainerState),
    {
        let prev_min_interest = self.min_interest();
        action(self);
        let new_min_interest = self.min_interest();
        if prev_min_interest == FidlInterest::default()
            || compare_fidl_interest(&new_min_interest, &prev_min_interest) != Ordering::Equal
        {
            debug!(%identity, ?new_min_interest, "Updating interest.");
            for value in self.hanging_gets.values_mut() {
                let locked = value.lock().take();
                if let Some(value) = locked {
                    let _ = value.send(Ok(new_min_interest.clone()));
                }
            }
        }
    }

    /// Pushes the given `interest` to the set.
    fn push_interest(&mut self, interest: FidlInterest) {
        if interest != FidlInterest::default() {
            let count = self.interests.entry(interest.into()).or_insert(0);
            *count += 1;
        }
    }

    /// Removes the given `interest` from the set
    fn erase(&mut self, interest: &FidlInterest) {
        let interest = interest.clone().into();
        if let Some(count) = self.interests.get_mut(&interest) {
            if *count <= 1 {
                self.interests.remove(&interest);
            } else {
                *count -= 1;
            }
        }
    }

    /// Returns a copy of the lowest interest in the set. If the set is empty, an EMPTY interest is
    /// returned.
    fn min_interest(&self) -> FidlInterest {
        // btreemap: keys are sorted and ascending.
        self.interests.keys().next().map(|i| i.0.clone()).unwrap_or_default()
    }
}

#[derive(Debug, PartialEq)]
struct Interest(FidlInterest);

impl From<FidlInterest> for Interest {
    fn from(interest: FidlInterest) -> Interest {
        Interest(interest)
    }
}

impl From<FidlSeverity> for Interest {
    fn from(severity: FidlSeverity) -> Interest {
        Interest(FidlInterest { min_severity: Some(severity), ..Default::default() })
    }
}

impl std::ops::Deref for Interest {
    type Target = FidlInterest;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Eq for Interest {}

impl Ord for Interest {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.min_severity, other.min_severity) {
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => Ordering::Equal,
            (Some(a), Some(b)) => a.cmp(&b),
        }
    }
}

impl PartialOrd for Interest {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Compares the minimum severity of two interests.
fn compare_fidl_interest(a: &FidlInterest, b: &FidlInterest) -> Ordering {
    match (a.min_severity, b.min_severity) {
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => Ordering::Equal,
        (Some(a), Some(b)) => a.cmp(&b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::shared_buffer::SharedBuffer;
    use fidl_fuchsia_diagnostics::{ComponentSelector, Severity, StringSelector};
    use fidl_fuchsia_logger::{LogSinkMarker, LogSinkProxy};
    use fuchsia_async::MonotonicDuration;
    use fuchsia_inspect as inspect;
    use fuchsia_inspect_derive::WithInspect;
    use futures::channel::mpsc::UnboundedReceiver;
    use moniker::ExtendedMoniker;

    fn initialize_container(
        severity: Option<Severity>,
    ) -> (Arc<LogsArtifactsContainer>, LogSinkProxy, UnboundedReceiver<Task<()>>) {
        let identity = Arc::new(ComponentIdentity::new(
            ExtendedMoniker::parse_str("/foo/bar").unwrap(),
            "fuchsia-pkg://test",
        ));
        let stats = Arc::new(
            LogStreamStats::default()
                .with_inspect(inspect::component::inspector().root(), identity.moniker.to_string())
                .expect("failed to attach component log stats"),
        );
        let buffer = Arc::new(SharedBuffer::new(1024 * 1024, Box::new(|_| {})));
        let container = Arc::new(LogsArtifactsContainer::new(
            identity,
            std::iter::empty(),
            severity,
            Arc::clone(&stats),
            buffer.new_container_buffer(Arc::new(vec!["a"].into()), stats),
            None,
        ));
        // Connect out LogSink under test and take its events channel.
        let (sender, _recv) = mpsc::unbounded();
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<LogSinkMarker>().expect("create log sink");
        container.handle_log_sink(stream, sender);
        (container, proxy, _recv)
    }

    #[fuchsia::test]
    async fn update_interest() {
        // Sync path test (initial interest)
        let (container, log_sink, _sender) = initialize_container(None);
        // Get initial interest
        let initial_interest = log_sink.wait_for_interest_change().await.unwrap().unwrap();
        {
            let test_state = container.hanging_get_test_state.lock();
            assert_eq!(*test_state, TestState::NoRequest);
        }
        // Async (blocking) path test.
        assert_eq!(initial_interest.min_severity, None);
        let log_sink_clone = log_sink.clone();
        let interest_future =
            Task::spawn(async move { log_sink_clone.wait_for_interest_change().await });
        // Wait for the background task to get blocked to test the blocking case
        loop {
            fuchsia_async::Timer::new(MonotonicDuration::from_millis(200)).await;
            {
                let test_state = container.hanging_get_test_state.lock();
                if *test_state == TestState::Blocked {
                    break;
                }
            }
        }
        // We should see this interest update. This should unblock the hanging get.
        container.update_interest([interest(&["foo", "bar"], Some(Severity::Info))].iter(), &[]);

        // Verify we see the last interest we set.
        assert_eq!(interest_future.await.unwrap().unwrap().min_severity, Some(Severity::Info));

        // Issuing another hanging get should error out the first one
        let log_sink_clone = log_sink.clone();
        let interest_future =
            Task::spawn(async move { log_sink_clone.wait_for_interest_change().await });
        // Since spawn is async we need to wait for first future to block before starting second
        // Fuchsia Rust provides no ordering guarantees with respect to async tasks
        loop {
            fuchsia_async::Timer::new(MonotonicDuration::from_millis(200)).await;
            {
                let test_state = container.hanging_get_test_state.lock();
                if *test_state == TestState::Blocked {
                    break;
                }
            }
        }
        let _interest_future_2 =
            Task::spawn(async move { log_sink.wait_for_interest_change().await });
        match interest_future.await {
            Ok(Err(InterestChangeError::CalledTwice)) => {
                // pass test
            }
            _ => {
                panic!("Invoking a second interest listener on a channel should cancel the first one with an error.");
            }
        }
    }

    #[fuchsia::test]
    async fn initial_interest() {
        let (_container, log_sink, _sender) = initialize_container(Some(Severity::Info));
        let initial_interest = log_sink.wait_for_interest_change().await.unwrap().unwrap();
        assert_eq!(initial_interest.min_severity, Some(Severity::Info));
    }

    #[fuchsia::test]
    async fn interest_serverity_semantics() {
        let (container, log_sink, _sender) = initialize_container(None);
        let initial_interest = log_sink.wait_for_interest_change().await.unwrap().unwrap();
        assert_eq!(initial_interest.min_severity, None);
        // Set some interest.
        container.update_interest([interest(&["foo", "bar"], Some(Severity::Info))].iter(), &[]);
        assert_severity(&log_sink, Severity::Info).await;
        assert_interests(&container, [(Severity::Info, 1)]);

        // Sending a higher interest (WARN > INFO) has no visible effect, even if the new interest
        // (WARN) will be tracked internally until reset.
        container.update_interest([interest(&["foo", "bar"], Some(Severity::Warn))].iter(), &[]);
        assert_interests(&container, [(Severity::Info, 1), (Severity::Warn, 1)]);

        // Sending a lower interest (DEBUG < INFO) updates the previous one.
        container.update_interest([interest(&["foo", "bar"], Some(Severity::Debug))].iter(), &[]);
        assert_severity(&log_sink, Severity::Debug).await;
        assert_interests(
            &container,
            [(Severity::Debug, 1), (Severity::Info, 1), (Severity::Warn, 1)],
        );

        // Sending the same interest leads to tracking it twice, but no updates are sent since it's
        // the same minimum interest.
        container.update_interest([interest(&["foo", "bar"], Some(Severity::Debug))].iter(), &[]);
        assert_interests(
            &container,
            [(Severity::Debug, 2), (Severity::Info, 1), (Severity::Warn, 1)],
        );

        // The first reset does nothing, since the new minimum interest remains the same (we had
        // inserted twice, therefore we need to reset twice).
        container.reset_interest(&[interest(&["foo", "bar"], Some(Severity::Debug))]);
        assert_interests(
            &container,
            [(Severity::Debug, 1), (Severity::Info, 1), (Severity::Warn, 1)],
        );

        // The second reset causes a change in minimum interest -> now INFO.
        container.reset_interest(&[interest(&["foo", "bar"], Some(Severity::Debug))]);
        assert_severity(&log_sink, Severity::Info).await;
        assert_interests(&container, [(Severity::Info, 1), (Severity::Warn, 1)]);

        // If we pass a previous severity (INFO), then we undo it and set the new one (ERROR).
        // However, we get WARN since that's the minimum severity in the set.
        container.update_interest(
            [interest(&["foo", "bar"], Some(Severity::Error))].iter(),
            &[interest(&["foo", "bar"], Some(Severity::Info))],
        );
        assert_severity(&log_sink, Severity::Warn).await;
        assert_interests(&container, [(Severity::Error, 1), (Severity::Warn, 1)]);

        // When we reset warn, now we get ERROR since that's the minimum severity in the set.
        container.reset_interest(&[interest(&["foo", "bar"], Some(Severity::Warn))]);
        assert_severity(&log_sink, Severity::Error).await;
        assert_interests(&container, [(Severity::Error, 1)]);

        // When we reset ERROR , we get back to EMPTY since we have removed all interests from the
        // set.
        container.reset_interest(&[interest(&["foo", "bar"], Some(Severity::Error))]);
        assert_eq!(
            log_sink.wait_for_interest_change().await.unwrap().unwrap(),
            FidlInterest::default()
        );

        assert_interests(&container, []);
    }

    fn interest(moniker: &[&str], min_severity: Option<Severity>) -> LogInterestSelector {
        LogInterestSelector {
            selector: ComponentSelector {
                moniker_segments: Some(
                    moniker.iter().map(|s| StringSelector::ExactMatch(s.to_string())).collect(),
                ),
                ..Default::default()
            },
            interest: FidlInterest { min_severity, ..Default::default() },
        }
    }

    async fn assert_severity(proxy: &LogSinkProxy, severity: Severity) {
        assert_eq!(
            proxy.wait_for_interest_change().await.unwrap().unwrap().min_severity.unwrap(),
            severity
        );
    }

    fn assert_interests<const N: usize>(
        container: &LogsArtifactsContainer,
        severities: [(Severity, usize); N],
    ) {
        let mut expected_map = BTreeMap::new();
        expected_map.extend(IntoIterator::into_iter(severities).map(|(s, c)| {
            let interest = FidlInterest { min_severity: Some(s), ..Default::default() };
            (interest.into(), c)
        }));
        assert_eq!(expected_map, container.state.lock().interests);
    }
}
