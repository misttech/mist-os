// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::constants;
use crate::diagnostics::{GlobalConnectionStats, TRACE_CATEGORY};
use crate::identity::ComponentIdentity;
use crate::inspect::collector::{self as collector, InspectData};
use diagnostics_data::{self as schema, InspectHandleName};
use diagnostics_hierarchy::{DiagnosticsHierarchy, HierarchyMatcher};
use fidl::endpoints::Proxy;
use flyweights::FlyStr;
use fuchsia_async::{self as fasync, DurationExt, TimeoutExt};
use fuchsia_inspect::reader::snapshot::{Snapshot, SnapshotTree};
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::channel::oneshot;
use futures::{FutureExt, Stream};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tracing::warn;
use {
    fidl_fuchsia_inspect as finspect, fidl_fuchsia_io as fio, fuchsia_trace as ftrace,
    inspect_fidl_load as deprecated_inspect,
};

#[derive(Debug)]
pub enum InspectHandle {
    Tree {
        proxy: finspect::TreeProxy,
        name: Option<FlyStr>,
    },
    Directory {
        proxy: fio::DirectoryProxy,
    },
    Escrow {
        vmo: Arc<zx::Vmo>,
        name: Option<FlyStr>,
        token: finspect::EscrowToken,
        related_koid: zx::Koid,
    },
}

impl InspectHandle {
    fn is_closed(&self) -> bool {
        match self {
            Self::Directory { proxy } => proxy.as_channel().is_closed(),
            Self::Tree { proxy, .. } => proxy.as_channel().is_closed(),
            Self::Escrow { .. } => false,
        }
    }

    async fn on_closed(&self) -> Result<zx::Signals, zx::Status> {
        match self {
            Self::Tree { proxy, .. } => proxy.on_closed().await,
            Self::Directory { proxy, .. } => proxy.on_closed().await,
            Self::Escrow { token, .. } => {
                fasync::OnSignals::new(&token.token, fidl::Signals::OBJECT_PEER_CLOSED).await
            }
        }
    }

    pub fn koid(&self) -> zx::Koid {
        match self {
            Self::Directory { proxy } => {
                proxy.as_channel().as_handle_ref().get_koid().expect("DirectoryProxy has koid")
            }
            Self::Tree { proxy, .. } => {
                proxy.as_channel().as_handle_ref().get_koid().expect("TreeProxy has koid")
            }
            // We return the related koid to index based on it so that the retrieval is more
            // efficient.
            Self::Escrow { related_koid, .. } => *related_koid,
        }
    }

    pub fn tree<T: Into<FlyStr>>(proxy: finspect::TreeProxy, name: Option<T>) -> Self {
        InspectHandle::Tree { proxy, name: name.map(|n| n.into()) }
    }

    pub fn escrow<T: Into<FlyStr>>(
        vmo: zx::Vmo,
        token: finspect::EscrowToken,
        name: Option<T>,
    ) -> Self {
        let related_koid = token.token.basic_info().unwrap().related_koid;
        InspectHandle::Escrow {
            vmo: Arc::new(vmo),
            name: name.map(|n| n.into()),
            token,
            related_koid,
        }
    }

    pub fn directory(proxy: fio::DirectoryProxy) -> Self {
        InspectHandle::Directory { proxy }
    }
}

struct StoredInspectHandle {
    handle: Arc<InspectHandle>,
    // Whenever we delete the stored handle from the map, we can leverage dropping this oneshot to
    // trigger completion of the Task listening for related handle peer closed.
    _control: oneshot::Sender<()>,
}

#[derive(Default)]
pub struct InspectArtifactsContainer {
    /// One or more proxies that this container is configured for.
    inspect_handles: HashMap<zx::Koid, StoredInspectHandle>,
    on_closed_tasks: fasync::TaskGroup,
}

impl InspectArtifactsContainer {
    /// Remove a handle via its `koid` from the set of proxies managed by `self`.
    pub fn remove_handle(&mut self, koid: zx::Koid) -> (Option<Arc<InspectHandle>>, usize) {
        let stored = self.inspect_handles.remove(&koid);
        (stored.map(|stored| stored.handle), self.inspect_handles.len())
    }

    /// Push a new handle into the container.
    ///
    /// Returns `None` if the handle is a DirectoryProxy and there is already one tracked,
    /// as only single handles are supported in the DirectoryProxy case.
    pub fn push_handle(&mut self, handle: InspectHandle) -> Option<oneshot::Receiver<zx::Koid>> {
        if !self.inspect_handles.is_empty() && matches!(handle, InspectHandle::Directory { .. }) {
            return None;
        }
        let (control_snd, control_rcv) = oneshot::channel();
        let handle = Arc::new(handle);
        let stored = StoredInspectHandle { _control: control_snd, handle: Arc::clone(&handle) };
        let koid = handle.koid();
        self.inspect_handles.insert(koid, stored);
        let (task, rcv) = Self::on_closed_task(handle, koid, control_rcv);
        self.on_closed_tasks.add(task);
        Some(rcv)
    }

    /// Generate an `UnpopulatedInspectDataContainer` from the proxies managed by `self`.
    ///
    /// Returns `None` if there are no valid proxies.
    pub fn create_unpopulated(
        &self,
        identity: &Arc<ComponentIdentity>,
        matcher: Option<Arc<HierarchyMatcher>>,
    ) -> Option<UnpopulatedInspectDataContainer> {
        if self.inspect_handles.is_empty() {
            return None;
        }

        Some(UnpopulatedInspectDataContainer {
            identity: Arc::clone(identity),
            inspect_matcher: matcher,
            inspect_handles: self
                .inspect_handles
                .values()
                .map(|s| Arc::downgrade(&s.handle))
                .collect::<Vec<_>>(),
        })
    }

    fn on_closed_task(
        proxy: Arc<InspectHandle>,
        koid: zx::Koid,
        control_rcv: oneshot::Receiver<()>,
    ) -> (fasync::Task<()>, oneshot::Receiver<zx::Koid>) {
        let (snd, rcv) = oneshot::channel();
        (
            fasync::Task::spawn(async move {
                if !proxy.is_closed() {
                    let closed_fut = proxy.on_closed();
                    futures::pin_mut!(closed_fut);
                    let _ = futures::future::select(closed_fut, control_rcv).await;
                }
                let _ = snd.send(koid);
            }),
            rcv,
        )
    }
}

#[cfg(test)]
impl InspectArtifactsContainer {
    pub(crate) fn handles(&self) -> impl ExactSizeIterator<Item = &InspectHandle> {
        self.inspect_handles.values().map(|stored| stored.handle.as_ref())
    }
}

static TIMEOUT_MESSAGE: &str = "Exceeded per-component time limit for fetching diagnostics data";

#[derive(Debug)]
pub enum ReadSnapshot {
    Single(Snapshot),
    Tree(SnapshotTree),
    Finished(DiagnosticsHierarchy),
}

/// Packet containing a snapshot and all the metadata needed to
/// populate a diagnostics schema for that snapshot.
#[derive(Debug)]
pub struct SnapshotData {
    /// Optional name of the file or InspectSink proxy that created this snapshot.
    pub name: Option<InspectHandleName>,
    /// Timestamp at which this snapshot resolved or failed.
    pub timestamp: zx::Time,
    /// Errors encountered when processing this snapshot.
    pub errors: Vec<schema::InspectError>,
    /// Optional snapshot of the inspect hierarchy, in case reading fails
    /// and we have errors to share with client.
    pub snapshot: Option<ReadSnapshot>,
    /// Whether or not the data was escrowed.
    pub escrowed: bool,
}

impl SnapshotData {
    async fn new(
        name: Option<InspectHandleName>,
        data: InspectData,
        lazy_child_timeout: zx::Duration,
        identity: &ComponentIdentity,
        parent_trace_id: ftrace::Id,
    ) -> SnapshotData {
        let trace_id = ftrace::Id::random();
        let _trace_guard = ftrace::async_enter!(
            trace_id,
            TRACE_CATEGORY,
            c"SnapshotData::new",
            // An async duration cannot have multiple concurrent child async durations
            // so we include the nonce as metadata to manually determine relationship.
            "parent_trace_id" => u64::from(parent_trace_id),
            "trace_id" => u64::from(trace_id),
            "moniker" => identity.to_string().as_ref(),
            "filename" => name
                    .as_ref()
                    .and_then(InspectHandleName::as_filename)
                    .unwrap_or(""),
            "name" => name
                    .as_ref()
                    .and_then(InspectHandleName::as_name)
                    .unwrap_or("")
        );
        match data {
            InspectData::Tree(tree) => {
                let lazy_child_timeout =
                    Duration::from_nanos(lazy_child_timeout.into_nanos() as u64);
                match SnapshotTree::try_from_with_timeout(&tree, lazy_child_timeout).await {
                    Ok(snapshot_tree) => {
                        SnapshotData::successful(ReadSnapshot::Tree(snapshot_tree), name, false)
                    }
                    Err(e) => SnapshotData::failed(
                        schema::InspectError { message: format!("{e:?}") },
                        name,
                        false,
                    ),
                }
            }
            InspectData::DeprecatedFidl(inspect_proxy) => {
                match deprecated_inspect::load_hierarchy(inspect_proxy).await {
                    Ok(hierarchy) => {
                        SnapshotData::successful(ReadSnapshot::Finished(hierarchy), name, false)
                    }
                    Err(e) => SnapshotData::failed(
                        schema::InspectError { message: format!("{e:?}") },
                        name,
                        false,
                    ),
                }
            }
            InspectData::Vmo { data: vmo, escrowed } => match Snapshot::try_from(vmo.as_ref()) {
                Ok(snapshot) => {
                    SnapshotData::successful(ReadSnapshot::Single(snapshot), name, escrowed)
                }
                Err(e) => SnapshotData::failed(
                    schema::InspectError { message: format!("{e:?}") },
                    name,
                    escrowed,
                ),
            },
            InspectData::File(contents) => match Snapshot::try_from(contents) {
                Ok(snapshot) => {
                    SnapshotData::successful(ReadSnapshot::Single(snapshot), name, false)
                }
                Err(e) => SnapshotData::failed(
                    schema::InspectError { message: format!("{e:?}") },
                    name,
                    false,
                ),
            },
        }
    }

    // Constructs packet that timestamps and packages inspect snapshot for exfiltration.
    fn successful(
        snapshot: ReadSnapshot,
        name: Option<InspectHandleName>,
        escrowed: bool,
    ) -> SnapshotData {
        SnapshotData {
            name,
            timestamp: fasync::Time::now().into_zx(),
            errors: Vec::new(),
            snapshot: Some(snapshot),
            escrowed,
        }
    }

    // Constructs packet that timestamps and packages inspect snapshot failure for exfiltration.
    fn failed(
        error: schema::InspectError,
        name: Option<InspectHandleName>,
        escrowed: bool,
    ) -> SnapshotData {
        SnapshotData {
            name,
            timestamp: fasync::Time::now().into_zx(),
            errors: vec![error],
            snapshot: None,
            escrowed,
        }
    }
}

/// PopulatedInspectDataContainer is the container that
/// holds the actual Inspect data for a given component,
/// along with all information needed to transform that data
/// to be returned to the client.
pub struct PopulatedInspectDataContainer {
    pub identity: Arc<ComponentIdentity>,
    /// Vector of all the snapshots of inspect hierarchies under
    /// the diagnostics directory of the component identified by
    /// moniker, along with the metadata needed to populate
    /// this snapshot's diagnostics schema.
    pub snapshot: SnapshotData,
    /// Optional hierarchy matcher. If unset, the reader is running
    /// in all-access mode, meaning no matching or filtering is required.
    pub inspect_matcher: Option<Arc<HierarchyMatcher>>,
}

enum Status {
    Begin,
    Pending(VecDeque<(Option<InspectHandleName>, InspectData)>),
}

struct State {
    status: Status,
    unpopulated: Arc<UnpopulatedInspectDataContainer>,
    batch_timeout: zx::Duration,
    elapsed_time: zx::Duration,
    global_stats: Arc<GlobalConnectionStats>,
    trace_guard: Arc<Option<ftrace::AsyncScope>>,
    trace_id: ftrace::Id,
}

impl State {
    fn into_pending(
        self,
        pending: VecDeque<(Option<InspectHandleName>, InspectData)>,
        start_time: zx::Time,
    ) -> Self {
        Self {
            unpopulated: self.unpopulated,
            status: Status::Pending(pending),
            batch_timeout: self.batch_timeout,
            global_stats: self.global_stats,
            elapsed_time: self.elapsed_time + (zx::Time::get_monotonic() - start_time),
            trace_guard: self.trace_guard,
            trace_id: self.trace_id,
        }
    }

    fn add_elapsed_time(&mut self, start_time: zx::Time) {
        self.elapsed_time += zx::Time::get_monotonic() - start_time
    }

    async fn iterate(
        mut self,
        start_time: zx::Time,
    ) -> Option<(PopulatedInspectDataContainer, State)> {
        loop {
            match &mut self.status {
                Status::Begin => {
                    let data_map =
                        collector::populate_data_map(&self.unpopulated.inspect_handles).await;
                    self = self
                        .into_pending(data_map.into_iter().collect::<VecDeque<_>>(), start_time);
                }
                Status::Pending(ref mut pending) => match pending.pop_front() {
                    None => {
                        self.global_stats.record_component_duration(
                            self.unpopulated.identity.moniker.to_string(),
                            self.elapsed_time + (zx::Time::get_monotonic() - start_time),
                        );
                        return None;
                    }
                    Some((name, data)) => {
                        let snapshot = SnapshotData::new(
                            name,
                            data,
                            self.batch_timeout / constants::LAZY_NODE_TIMEOUT_PROPORTION,
                            &self.unpopulated.identity,
                            self.trace_id,
                        )
                        .await;
                        let result = PopulatedInspectDataContainer {
                            identity: Arc::clone(&self.unpopulated.identity),
                            snapshot,
                            inspect_matcher: self.unpopulated.inspect_matcher.clone(),
                        };
                        self.add_elapsed_time(start_time);
                        return Some((result, self));
                    }
                },
            }
        }
    }
}

/// UnpopulatedInspectDataContainer is the container that holds
/// all information needed to retrieve Inspect data
/// for a given component, when requested.
#[derive(Debug)]
pub struct UnpopulatedInspectDataContainer {
    pub identity: Arc<ComponentIdentity>,
    /// Proxies configured for container. It is an invariant that if any value is an
    /// InspectHandle::Directory, then there is exactly one value.
    pub inspect_handles: Vec<Weak<InspectHandle>>,
    /// Optional hierarchy matcher. If unset, the reader is running
    /// in all-access mode, meaning no matching or filtering is required.
    pub inspect_matcher: Option<Arc<HierarchyMatcher>>,
}

impl UnpopulatedInspectDataContainer {
    /// Populates this data container with a timeout. On the timeout firing returns a
    /// container suitable to return to clients, but with timeout error information recorded.
    pub fn populate(
        self,
        timeout: i64,
        global_stats: Arc<GlobalConnectionStats>,
        parent_trace_id: ftrace::Id,
    ) -> impl Stream<Item = PopulatedInspectDataContainer> {
        let trace_id = ftrace::Id::random();
        let trace_guard = ftrace::async_enter!(
            trace_id,
            TRACE_CATEGORY,
            c"ReaderServer::stream.populate",
            // An async duration cannot have multiple concurrent child async durations
            // so we include the nonce as metadata to manually determine relationship.
            "parent_trace_id" => u64::from(parent_trace_id),
            "trace_id" => u64::from(trace_id),
            "moniker" => self.identity.to_string().as_ref()
        );
        let this = Arc::new(self);
        let state = State {
            status: Status::Begin,
            unpopulated: this,
            batch_timeout: zx::Duration::from_seconds(timeout),
            global_stats,
            elapsed_time: zx::Duration::from_nanos(0),
            trace_guard: Arc::new(trace_guard),
            trace_id,
        };

        futures::stream::unfold(state, |state| {
            let unpopulated_for_timeout = Arc::clone(&state.unpopulated);
            let timeout = state.batch_timeout;
            let elapsed_time = state.elapsed_time;
            let global_stats = Arc::clone(&state.global_stats);
            let start_time = zx::Time::get_monotonic();
            let trace_guard = Arc::clone(&state.trace_guard);
            let trace_id = state.trace_id;

            state
                .iterate(start_time)
                .on_timeout((timeout - elapsed_time).after_now(), move || {
                    warn!(identity = ?unpopulated_for_timeout.identity.moniker, "{TIMEOUT_MESSAGE}");
                    global_stats.add_timeout();
                    let result = PopulatedInspectDataContainer {
                        identity: Arc::clone(&unpopulated_for_timeout.identity),
                        inspect_matcher: unpopulated_for_timeout.inspect_matcher.clone(),
                        snapshot: SnapshotData::failed(
                            schema::InspectError { message: TIMEOUT_MESSAGE.to_string() },
                            None,
                            false,
                        ),
                    };
                    Some((
                        result,
                        State {
                            status: Status::Pending(VecDeque::new()),
                            unpopulated: unpopulated_for_timeout,
                            batch_timeout: timeout,
                            global_stats,
                            elapsed_time: elapsed_time + (zx::Time::get_monotonic() - start_time),
                            trace_guard,
                            trace_id,
                        },
                    ))
                })
                .boxed()
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fuchsia_inspect::Node;
    use fuchsia_zircon::DurationNum;
    use futures::StreamExt;
    use once_cell::sync::Lazy;

    static EMPTY_IDENTITY: Lazy<Arc<ComponentIdentity>> =
        Lazy::new(|| Arc::new(ComponentIdentity::unknown()));

    #[fuchsia::test]
    async fn population_times_out() {
        // Simulate a directory that hangs indefinitely in any request so that we consistently
        // trigger the 0 timeout.
        let (directory, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<fio::DirectoryMarker>().unwrap();
        fasync::Task::spawn(async move {
            while stream.next().await.is_some() {
                fasync::Timer::new(fasync::Time::after(100000.second())).await;
            }
        })
        .detach();

        let handle = Arc::new(InspectHandle::directory(directory));
        let container = UnpopulatedInspectDataContainer {
            identity: EMPTY_IDENTITY.clone(),
            inspect_handles: vec![Arc::downgrade(&handle)],
            inspect_matcher: None,
        };
        let mut stream = container.populate(
            0,
            Arc::new(GlobalConnectionStats::new(Node::default())),
            ftrace::Id::random(),
        );
        let res = stream.next().await.unwrap();
        assert_eq!(res.snapshot.name, None);
        assert_eq!(
            res.snapshot.errors,
            vec![schema::InspectError { message: TIMEOUT_MESSAGE.to_string() }]
        );
    }

    #[fuchsia::test]
    async fn no_inspect_files_do_not_give_an_error_response() {
        let directory = Arc::new(InspectHandle::directory(
            fuchsia_fs::directory::open_in_namespace("/tmp", fuchsia_fs::OpenFlags::RIGHT_READABLE)
                .unwrap(),
        ));
        let container = UnpopulatedInspectDataContainer {
            identity: EMPTY_IDENTITY.clone(),
            inspect_handles: vec![Arc::downgrade(&directory)],
            inspect_matcher: None,
        };
        let mut stream = container.populate(
            1000000,
            Arc::new(GlobalConnectionStats::new(Node::default())),
            ftrace::Id::random(),
        );
        assert!(stream.next().await.is_none());
    }

    #[fuchsia::test]
    fn only_one_directory_proxy_is_populated() {
        let _executor = fuchsia_async::LocalExecutor::new();
        let (directory, _) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let mut container = InspectArtifactsContainer::default();
        let _rx = container.push_handle(InspectHandle::directory(directory));
        let (directory2, _) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        assert!(container.push_handle(InspectHandle::directory(directory2)).is_none());
    }
}
