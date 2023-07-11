// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    constants,
    diagnostics::GlobalConnectionStats,
    identity::ComponentIdentity,
    inspect::collector::{self as collector, InspectData},
};
use diagnostics_data::{self as schema, InspectHandleName};
use diagnostics_hierarchy::{DiagnosticsHierarchy, HierarchyMatcher};
use fidl::endpoints::Proxy;
use fidl_fuchsia_inspect::TreeProxy;
use fidl_fuchsia_io as fio;
use fuchsia_async::{self as fasync, DurationExt, TimeoutExt};
use fuchsia_fs;
use fuchsia_inspect::reader::snapshot::{Snapshot, SnapshotTree};
use fuchsia_trace as ftrace;
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::{channel::oneshot, FutureExt, Stream};
use inspect_fidl_load as deprecated_inspect;
use lazy_static::lazy_static;
use std::time::Duration;
use std::{
    collections::{HashMap, VecDeque},
    convert::{From, Into, TryFrom},
    sync::Arc,
};
use tracing::warn;

#[derive(Debug, Clone)]
pub enum InspectHandle {
    Tree(TreeProxy, Option<InspectHandleName>),
    Directory(fio::DirectoryProxy),
}

impl InspectHandle {
    pub fn is_closed(&self) -> bool {
        match self {
            Self::Directory(proxy) => proxy.as_channel().is_closed(),
            Self::Tree(proxy, _) => proxy.as_channel().is_closed(),
        }
    }

    pub async fn on_closed(&self) -> Result<zx::Signals, zx::Status> {
        match self {
            Self::Tree(proxy, _) => proxy.on_closed().await,
            Self::Directory(proxy) => proxy.on_closed().await,
        }
    }

    pub fn koid(&self) -> zx::Koid {
        match self {
            Self::Directory(proxy) => {
                proxy.as_channel().as_handle_ref().get_koid().expect("DirectoryProxy has koid")
            }
            Self::Tree(proxy, _) => {
                proxy.as_channel().as_handle_ref().get_koid().expect("TreeProxy has koid")
            }
        }
    }

    pub fn from_named_tree_proxy(proxy: TreeProxy, name: Option<String>) -> Self {
        InspectHandle::Tree(proxy, name.map(InspectHandleName::name))
    }

    pub fn is_tree(&self) -> bool {
        matches!(self, Self::Tree(_, _))
    }
}

impl From<fio::DirectoryProxy> for InspectHandle {
    fn from(proxy: fio::DirectoryProxy) -> Self {
        InspectHandle::Directory(proxy)
    }
}

impl From<TreeProxy> for InspectHandle {
    fn from(proxy: TreeProxy) -> Self {
        InspectHandle::Tree(proxy, None)
    }
}

#[derive(Debug)]
pub struct InspectArtifactsContainer {
    /// One or more proxies that this container is configured for.
    inspect_handles: HashMap<zx::Koid, Arc<InspectHandle>>,
    on_closed_tasks: Vec<fasync::Task<()>>,
}

impl InspectArtifactsContainer {
    /// Create a new `InspectArtifactsContainer`.
    ///
    /// Returns itself and a `Receiver` that resolves into the koid of the input `proxy`
    /// when that proxy closes.
    pub fn new(proxy: impl Into<InspectHandle>) -> (Self, oneshot::Receiver<zx::Koid>) {
        let mut this = Self { inspect_handles: HashMap::new(), on_closed_tasks: vec![] };
        // safe to unwrap as diagnostics_proxy is guaranteed to be empty
        let rx = this.push_handle(proxy).unwrap();
        (this, rx)
    }

    /// Remove a handle via its `koid` from the set of proxies managed by `self`.
    pub fn remove_handle(&mut self, koid: zx::Koid) -> usize {
        self.inspect_handles.remove(&koid);
        self.inspect_handles.len()
    }

    /// Push a new handle into the container.
    ///
    /// Returns `None` if the handle is a DirectoryProxy and there is already one tracked,
    /// as only single handles are supported in the DirectoryProxy case.
    pub fn push_handle(
        &mut self,
        new_handle: impl Into<InspectHandle>,
    ) -> Option<oneshot::Receiver<zx::Koid>> {
        let handle = new_handle.into();
        if !self.inspect_handles.is_empty() && matches!(handle, InspectHandle::Directory(_)) {
            return None;
        }

        let handle = Arc::new(handle);
        let koid = handle.koid();
        self.inspect_handles.insert(koid, Arc::clone(&handle));
        let (task, rcv) = Self::on_closed_task(handle, koid);
        self.on_closed_tasks.push(task);
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

        // we know there is at least 1
        let handle = self.inspect_handles.values().next().unwrap();

        match handle.as_ref() {
            // there can only be one Directory, semantically
            InspectHandle::Directory(dir) if self.inspect_handles.len() == 1 => {
                fuchsia_fs::directory::clone_no_describe(dir, None).ok().map(|directory| {
                    UnpopulatedInspectDataContainer {
                        identity: Arc::clone(identity),
                        inspect_handles: vec![directory.into()],
                        inspect_matcher: matcher,
                    }
                })
            }

            InspectHandle::Tree(_, _) => Some(UnpopulatedInspectDataContainer {
                identity: Arc::clone(identity),
                inspect_matcher: matcher,
                inspect_handles: self
                    .inspect_handles
                    .values()
                    .filter(|handle| handle.as_ref().is_tree())
                    .map(|handle| handle.as_ref().clone())
                    .collect::<Vec<InspectHandle>>(),
            }),

            _ => None,
        }
    }

    fn on_closed_task(
        proxy: Arc<InspectHandle>,
        koid: zx::Koid,
    ) -> (fasync::Task<()>, oneshot::Receiver<zx::Koid>) {
        let (snd, rcv) = oneshot::channel();
        (
            fasync::Task::spawn(async move {
                if !proxy.is_closed() {
                    let _ = proxy.on_closed().await;
                }
                let _ = snd.send(koid);
            }),
            rcv,
        )
    }
}

#[cfg(test)]
impl InspectArtifactsContainer {
    pub fn get_handle_by_koid(&self, koid: &zx::Koid) -> Option<Arc<InspectHandle>> {
        self.inspect_handles.get(koid).cloned()
    }

    pub fn len(&self) -> usize {
        self.inspect_handles.len()
    }
}

lazy_static! {
    static ref TIMEOUT_MESSAGE: &'static str =
        "Exceeded per-component time limit for fetching diagnostics data";
}

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
            "app",
            "SnapshotData::new",
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
                        SnapshotData::successful(ReadSnapshot::Tree(snapshot_tree), name)
                    }
                    Err(e) => SnapshotData::failed(
                        schema::InspectError { message: format!("{e:?}") },
                        name,
                    ),
                }
            }
            InspectData::DeprecatedFidl(inspect_proxy) => {
                match deprecated_inspect::load_hierarchy(inspect_proxy).await {
                    Ok(hierarchy) => {
                        SnapshotData::successful(ReadSnapshot::Finished(hierarchy), name)
                    }
                    Err(e) => SnapshotData::failed(
                        schema::InspectError { message: format!("{e:?}") },
                        name,
                    ),
                }
            }
            InspectData::Vmo(vmo) => match Snapshot::try_from(&vmo) {
                Ok(snapshot) => SnapshotData::successful(ReadSnapshot::Single(snapshot), name),
                Err(e) => {
                    SnapshotData::failed(schema::InspectError { message: format!("{e:?}") }, name)
                }
            },
            InspectData::File(contents) => match Snapshot::try_from(contents) {
                Ok(snapshot) => SnapshotData::successful(ReadSnapshot::Single(snapshot), name),
                Err(e) => {
                    SnapshotData::failed(schema::InspectError { message: format!("{e:?}") }, name)
                }
            },
        }
    }

    // Constructs packet that timestamps and packages inspect snapshot for exfiltration.
    fn successful(snapshot: ReadSnapshot, name: Option<InspectHandleName>) -> SnapshotData {
        SnapshotData {
            name,
            timestamp: fasync::Time::now().into_zx(),
            errors: Vec::new(),
            snapshot: Some(snapshot),
        }
    }

    // Constructs packet that timestamps and packages inspect snapshot failure for exfiltration.
    fn failed(error: schema::InspectError, name: Option<InspectHandleName>) -> SnapshotData {
        SnapshotData {
            name,
            timestamp: fasync::Time::now().into_zx(),
            errors: vec![error],
            snapshot: None,
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
    batch_timeout: Option<zx::Duration>,
    elapsed_time: zx::Duration,
    global_stats: Arc<GlobalConnectionStats>,
    trace_guard: Arc<ftrace::AsyncScope>,
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
                        self.global_stats
                            .record_component_duration(
                                self.unpopulated.identity.moniker.to_string(),
                                self.elapsed_time + (zx::Time::get_monotonic() - start_time),
                            )
                            .await;
                        return None;
                    }
                    Some((name, data)) => {
                        let snapshot = SnapshotData::new(
                            name,
                            data,
                            self.batch_timeout.unwrap_or(zx::Duration::from_seconds(
                                constants::PER_COMPONENT_ASYNC_TIMEOUT_SECONDS,
                            )) / constants::LAZY_NODE_TIMEOUT_PROPORTION,
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
    pub inspect_handles: Vec<InspectHandle>,
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
            "app",
            "ReaderServer::stream.populate",
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
            batch_timeout: Some(zx::Duration::from_seconds(timeout)),
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

            let fut = state.iterate(start_time);
            match timeout {
                None => fut.boxed(),
                Some(timeout) => fut
                    .on_timeout((timeout - elapsed_time).after_now(), move || {
                        warn!(identity = ?unpopulated_for_timeout.identity.moniker,
                            "{}", &*TIMEOUT_MESSAGE);
                        global_stats.add_timeout();
                        let result = PopulatedInspectDataContainer {
                            identity: Arc::clone(&unpopulated_for_timeout.identity),
                            inspect_matcher: unpopulated_for_timeout.inspect_matcher.clone(),
                            snapshot: SnapshotData::failed(
                                schema::InspectError { message: TIMEOUT_MESSAGE.to_string() },
                                None,
                            ),
                        };
                        Some((
                            result,
                            State {
                                status: Status::Pending(VecDeque::new()),
                                unpopulated: unpopulated_for_timeout,
                                batch_timeout: None,
                                global_stats,
                                elapsed_time: elapsed_time
                                    + (zx::Time::get_monotonic() - start_time),
                                trace_guard,
                                trace_id,
                            },
                        ))
                    })
                    .boxed(),
            }
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::diagnostics::GlobalConnectionStats;
    use fuchsia_inspect::Node;
    use fuchsia_zircon::DurationNum;
    use futures::StreamExt;

    lazy_static! {
        static ref EMPTY_IDENTITY: Arc<ComponentIdentity> = Arc::new(ComponentIdentity::unknown());
    }

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

        let container = UnpopulatedInspectDataContainer {
            identity: EMPTY_IDENTITY.clone(),
            inspect_handles: vec![directory.into()],
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
        let directory =
            fuchsia_fs::directory::open_in_namespace("/tmp", fuchsia_fs::OpenFlags::RIGHT_READABLE)
                .unwrap();
        let container = UnpopulatedInspectDataContainer {
            identity: EMPTY_IDENTITY.clone(),
            inspect_handles: vec![directory.into()],
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
    async fn only_one_directory_proxy_is_populated() {
        let (directory, _) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let (mut container, _rx) = InspectArtifactsContainer::new(directory);
        let (directory2, _) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        assert!(container.push_handle(directory2).is_none());
    }
}
