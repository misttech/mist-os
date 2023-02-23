// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilityProvider,
        model::{
            component::{ComponentInstance, WeakComponentInstance},
            error::ModelError,
            hooks::{Event, EventPayload, EventType, Hook, HooksRegistration},
            mutable_directory::MutableDirectory,
            routing::{open_capability_at_source, CapabilitySource, OpenRequest, RoutingError},
        },
    },
    async_trait::async_trait,
    cm_rust::{CapabilityName, CapabilityTypeName, ComponentDecl, ExposeDecl, ExposeDeclCommon},
    cm_task_scope::TaskScope,
    cm_util::channel,
    fidl::{endpoints::ServerEnd, epitaph::ChannelEpitaphExt},
    fidl_fuchsia_io as fio,
    fuchsia_async::{DurationExt, TimeoutExt},
    fuchsia_zircon as zx,
    futures::future::{join_all, BoxFuture},
    futures::lock::Mutex,
    futures::FutureExt,
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase, ExtendedMoniker},
    routing::capability_source::AggregateCapabilityProvider,
    std::{
        collections::{HashMap, HashSet},
        path::PathBuf,
        sync::{Arc, Weak},
    },
    tracing::{error, warn},
    vfs::{
        common::send_on_open_with_error,
        directory::{
            connection::io1::DerivedConnection,
            dirents_sink,
            entry::{DirectoryEntry, EntryInfo},
            entry_container::{Directory, DirectoryWatcher},
            immutable::connection::io1::ImmutableConnection,
            immutable::lazy as lazy_immutable_dir,
            immutable::simple::{simple as simple_immutable_dir, Simple as SimpleImmutableDir},
            traversal_position::TraversalPosition,
        },
        execution_scope::ExecutionScope,
    },
};

/// Timeout for opening a service capability when aggregating.
const OPEN_SERVICE_TIMEOUT: zx::Duration = zx::Duration::from_seconds(5);

/// Provides a Service capability where the target component only has access to a subset of the
/// Service instances exposed by the source component.
pub struct FilteredServiceProvider {
    /// Execution scope for requests to `dir`. This is the same scope
    /// as the one in `collection_component`'s resolved state.
    execution_scope: ExecutionScope,

    /// Set of service instance names that are available to the target component.
    source_instance_filter: HashSet<String>,

    /// Mapping of service instance names in the source component to new names in the target.
    instance_name_source_to_target: HashMap<String, Vec<String>>,

    /// The underlying un-filtered service capability provider.
    source_service_provider: Box<dyn CapabilityProvider>,
}

impl FilteredServiceProvider {
    pub async fn new(
        source_component: &Arc<ComponentInstance>,
        source_instances: Vec<String>,
        instance_name_source_to_target: HashMap<String, Vec<String>>,
        source_service_provider: Box<dyn CapabilityProvider>,
    ) -> Result<Self, ModelError> {
        let execution_scope =
            source_component.lock_resolved_state().await?.execution_scope().clone();
        Ok(FilteredServiceProvider {
            execution_scope,
            source_instance_filter: source_instances.into_iter().collect(),
            instance_name_source_to_target,
            source_service_provider,
        })
    }
}

/// The virtual directory implementation used by the FilteredServiceProvider to host the
/// filtered set of service instances available to the target component.
pub struct FilteredServiceDirectory {
    source_instance_filter: HashSet<String>,
    source_dir_proxy: fio::DirectoryProxy,
    instance_name_source_to_target: HashMap<String, Vec<String>>,
    instance_name_target_to_source: HashMap<String, String>,
}

impl FilteredServiceDirectory {
    /// Returns true if the requested path matches an allowed instance.
    pub fn path_matches_allowed_instance(self: &Self, target_path: &String) -> bool {
        if target_path.is_empty() {
            return false;
        }
        if self.source_instance_filter.is_empty() {
            // If an instance was renamed the original name shouldn't be usable.
            !self.instance_name_source_to_target.contains_key(target_path)
        } else {
            self.source_instance_filter.contains(target_path)
        }
    }
}

#[async_trait]
impl DirectoryEntry for FilteredServiceDirectory {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: vfs::path::Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        if path.is_empty() {
            // If path is empty just connect to itself as a directory.
            ImmutableConnection::create_connection(scope, self, flags, server_end);
            return;
        }
        let input_path_string = path.clone().into_string();
        let service_instance_name =
            path.peek().map(ToString::to_string).expect("path should not be empty");
        if self.path_matches_allowed_instance(&service_instance_name) {
            let source_path = self
                .instance_name_target_to_source
                .get(&service_instance_name)
                .map_or(input_path_string, |source| {
                    // Valid paths are "<instance_name>" or "<instance_name>/<protocol_name>".
                    // If the incoming path is just a service instance name return the source component instance name.
                    if path.is_single_component() {
                        return source.to_string();
                    }
                    let mut mut_path = path.clone();
                    // If the incoming path is "<instance_name>/<protocol_name>" replace <instance_name> with the source component instance name.
                    mut_path.next();
                    // Since we check above if the path is a single component it's safe to unwrap here.
                    let protocol_name = mut_path.next().unwrap();
                    format!("{}/{}", source, protocol_name).to_string()
                });

            if let Err(e) =
                self.source_dir_proxy.open(flags, fio::ModeType::empty(), &source_path, server_end)
            {
                error!(
                    error = %e,
                    path = source_path.as_str(),
                    "Error opening instance in FilteredServiceDirectory"
                );
            }
            return;
        } else {
            send_on_open_with_error(flags, server_end, zx::Status::NOT_FOUND);
        }
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }
}

#[async_trait]
impl Directory for FilteredServiceDirectory {
    async fn read_dirents<'a>(
        &'a self,
        pos: &'a TraversalPosition,
        mut sink: Box<dyn dirents_sink::Sink>,
    ) -> Result<(TraversalPosition, Box<dyn dirents_sink::Sealed>), zx::Status> {
        let next_entry = match pos {
            TraversalPosition::End => {
                // Bail out early when there is no work to do.
                // This method is always called at least once with TraversalPosition::End.
                return Ok((TraversalPosition::End, sink.seal()));
            }
            TraversalPosition::Start => None,
            TraversalPosition::Name(entry) => Some(entry),
            TraversalPosition::Index(_) => panic!("TraversalPosition::Index is never used"),
        };
        match fuchsia_fs::directory::readdir(&self.source_dir_proxy).await {
            Ok(dirent_vec) => {
                for dirent in dirent_vec {
                    let entry_name = dirent.name;
                    // If entry_name is the same as one of the target values in an instance rename, skip it.
                    // we prefer target renames over original source instance names in the case of a conflict.
                    if let Some(source_name) = self.instance_name_target_to_source.get(&entry_name)
                    {
                        // If the source and target name are the same, then there is no conflict to resolve,
                        // and we allow the no-op source to target name translation to happen below.
                        if source_name != &entry_name {
                            continue;
                        }
                    }

                    let target_entry_name_vec = self
                        .instance_name_source_to_target
                        .get(&entry_name)
                        .map_or(vec![entry_name.clone()], |t_names| t_names.clone());

                    for target_entry_name in target_entry_name_vec {
                        // Only reveal allowed source instances,
                        if !self.path_matches_allowed_instance(&target_entry_name) {
                            continue;
                        }
                        if let Some(next_entry_name) = next_entry {
                            if target_entry_name < next_entry_name.to_string() {
                                continue;
                            }
                        }
                        sink = match sink.append(
                            &EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory),
                            &target_entry_name,
                        ) {
                            dirents_sink::AppendResult::Ok(sink) => sink,
                            dirents_sink::AppendResult::Sealed(sealed) => {
                                // There is not enough space to return this entry. Record it as the next
                                // entry to start at for subsequent calls.
                                return Ok((
                                    TraversalPosition::Name(target_entry_name.clone()),
                                    sealed,
                                ));
                            }
                        }
                    }
                }
            }
            Err(error) => {
                warn!(%error, "Error reading the source components service directory");
                return Err(zx::Status::INTERNAL);
            }
        }
        return Ok((TraversalPosition::End, sink.seal()));
    }

    fn register_watcher(
        self: Arc<Self>,
        _scope: ExecutionScope,
        _mask: fio::WatchMask,
        _channel: DirectoryWatcher,
    ) -> Result<(), zx::Status> {
        // TODO(fxb/96023) implement watcher behavior.
        Err(zx::Status::NOT_SUPPORTED)
    }

    fn unregister_watcher(self: Arc<Self>, _key: usize) {
        // TODO(fxb/96023) implement watcher behavior.
    }

    /// Get this directory's attributes.
    /// The "mode" field will be filled in by the connection.
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_DIRECTORY,
            id: fio::INO_UNKNOWN,
            content_size: 0,
            storage_size: 0,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }

    /// Called when the directory is closed.
    fn close(&self) -> Result<(), zx::Status> {
        Ok(())
    }
}

#[async_trait]
impl CapabilityProvider for FilteredServiceProvider {
    async fn open(
        mut self: Box<Self>,
        task_scope: TaskScope,
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), ModelError> {
        let relative_path_utf8 = relative_path
            .to_str()
            .ok_or_else(|| ModelError::path_is_not_utf8(relative_path.clone()))?;
        let relative_path_vfs = if relative_path_utf8.is_empty() {
            vfs::path::Path::dot()
        } else {
            vfs::path::Path::validate_and_split(relative_path_utf8)
                .map_err(|_| ModelError::path_invalid(relative_path_utf8))?
        };

        // Create a remote directory referring to the unfiltered source service directory.
        let (source_service_proxy, source_service_server_end) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        self.source_service_provider
            .open(
                task_scope,
                // Open the directory with client-provided flags, but add DIRECTORY and remove
                // NOT_DIRECTORY.
                //
                // Normally, opening this directory only requires the DIRECTORY right. The client
                // may be trying to open a non-directory node within the service directory (i.e.
                // a protocol in a service instance), so the flags cannot be used as-is.
                flags & !fio::OpenFlags::NOT_DIRECTORY | fio::OpenFlags::DIRECTORY,
                PathBuf::new(),
                &mut (source_service_server_end.into_channel()),
            )
            .await?;

        let instance_name_target_to_source = {
            let mut m = HashMap::new();
            // We want to ensure that there is a one-to-many mapping from
            // source to target names, with no target names being used for multiple source names.
            // This is validated by cm_fidl_validator, so we can safely panic and not proceed
            // if that is violated here.
            for (k, v) in self.instance_name_source_to_target.iter() {
                for name in v {
                    if m.insert(name.clone(), k.clone()).is_some() {
                        panic!(
                            "duplicate target name found in instance_name_source_to_target, name: {:?}",
                            v
                        );
                    }
                }
            }
            m
        };
        // Arc to FilteredServiceDirectory will stay in scope and will usable by the caller
        // because the underlying implementation of Arc<FilteredServiceDirectory>.open
        // does one of 3 things depending on the path argument.
        // 1. We open an actual instance, which forwards the open call to the directory entry in the source (original unfiltered) service providing instance.
        // 2. The path is unknown/ not in the directory and we error.
        // 3. we call ImmutableConnection::create_connection with self as one of the arguments.
        // In case 3 the directory will stay in scope until the server_end channel is closed.
        Arc::new(FilteredServiceDirectory {
            source_instance_filter: self.source_instance_filter,
            source_dir_proxy: source_service_proxy,
            instance_name_target_to_source,
            instance_name_source_to_target: self.instance_name_source_to_target,
        })
        .open(
            self.execution_scope.clone(),
            flags,
            relative_path_vfs,
            ServerEnd::new(channel::take_channel(server_end)),
        );
        Ok(())
    }
}

/// Serves a Service directory that allows clients to list instances resulting from an aggregation of service offers
/// and to open instances.
///
pub struct AggregateServiceDirectoryProvider {
    /// Execution scope for requests to `dir`. This is the same scope
    /// as the one in `collection_component`'s resolved state.
    execution_scope: ExecutionScope,

    /// The directory that contains entries for all service instances
    /// across all of the aggregated source services.
    dir: Arc<lazy_immutable_dir::Lazy<AggregateServiceDirectory>>,
}

impl AggregateServiceDirectoryProvider {
    pub async fn create(
        parent: WeakComponentInstance,
        target: WeakComponentInstance,
        provider: Box<dyn AggregateCapabilityProvider<ComponentInstance>>,
    ) -> Result<AggregateServiceDirectoryProvider, ModelError> {
        let execution_scope =
            parent.upgrade()?.lock_resolved_state().await?.execution_scope().clone();
        let dir = lazy_immutable_dir::lazy(AggregateServiceDirectory {
            parent: parent.clone(),
            target,
            provider,
        });
        Ok(AggregateServiceDirectoryProvider { execution_scope, dir })
    }
}

#[async_trait]
impl CapabilityProvider for AggregateServiceDirectoryProvider {
    async fn open(
        self: Box<Self>,
        _task_scope: TaskScope,
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), ModelError> {
        let relative_path_utf8 = relative_path
            .to_str()
            .ok_or_else(|| ModelError::path_is_not_utf8(relative_path.clone()))?;
        let relative_path = if relative_path_utf8.is_empty() {
            vfs::path::Path::dot()
        } else {
            vfs::path::Path::validate_and_split(relative_path_utf8)
                .map_err(|_| ModelError::path_invalid(relative_path_utf8))?
        };
        self.dir.open(
            self.execution_scope.clone(),
            flags,
            relative_path,
            ServerEnd::new(channel::take_channel(server_end)),
        );
        Ok(())
    }
}

/// A directory entry representing a service with multiple services as its source.
/// This directory is hosted by component_manager on behalf of the component which offered multiple sources of
/// the same service capability.
///
/// This directory can be accessed by components by opening `/svc/my.service/` in their
/// incoming namespace when they have a `use my.service` declaration in their manifest, and the
/// source of `my.service` is multiple services.
struct AggregateServiceDirectory {
    /// The parent component of the collection and aggregated service.
    parent: WeakComponentInstance,
    /// The original target of the capability route (the component that opened this directory).
    target: WeakComponentInstance,
    /// The provider that lists collection instances and performs routing to an instance.
    provider: Box<dyn AggregateCapabilityProvider<ComponentInstance>>,
}

#[async_trait]
impl lazy_immutable_dir::LazyDirectory for AggregateServiceDirectory {
    async fn get_entry(&self, name: &str) -> Result<Arc<dyn DirectoryEntry>, zx::Status> {
        // Parse the entry name into its (component,instance) parts.
        // In the case of non-comma separated entries, treat the component and
        // instance name as the same.
        let (component, instance) = name.split_once(',').unwrap_or((name, name));

        let source = match self.provider.route_instance(&component).await {
            Ok(source) => Ok(source),
            Err(error) => {
                let parent = self.parent.upgrade().map_err(|e| e.as_zx_status())?;
                let target = self.target.upgrade().map_err(|e| e.as_zx_status())?;
                target
                    .with_logger_as_default(|| {
                        warn!(
                            component, instance, parent=%parent.abs_moniker, %error,
                            "Failed to route aggregate service instance",
                        );
                    })
                    .await;
                Err(zx::Status::NOT_FOUND)
            }
        }?;

        Ok(Arc::new(ServiceInstanceDirectoryEntry {
            name: name.to_string(),
            source,
            source_component_name: component.to_string(),
            service_instance: instance.to_string(),
            parent: self.parent.clone(),
        }))
    }

    async fn read_dirents<'a>(
        &'a self,
        pos: &'a TraversalPosition,
        mut sink: Box<dyn dirents_sink::Sink>,
    ) -> Result<(TraversalPosition, Box<dyn dirents_sink::Sealed>), zx::Status> {
        let next_entry = match pos {
            TraversalPosition::End => {
                // Bail out early when there is no work to do.
                // This method is always called at least once with TraversalPosition::End.
                return Ok((TraversalPosition::End, sink.seal()));
            }
            TraversalPosition::Start => None,
            TraversalPosition::Name(entry) => {
                // All generated filenames are guaranteed to have the ',' separator.
                entry.split_once(',').or(Some((entry.as_str(), entry.as_str())))
            }
            TraversalPosition::Index(_) => panic!("TraversalPosition::Index is never used"),
        };

        let target = self.target.upgrade().map_err(|e| e.as_zx_status())?;
        let mut instances =
            self.provider.list_instances().await.map_err(|_| zx::Status::INTERNAL)?;
        if instances.is_empty() {
            return Ok((TraversalPosition::End, sink.seal()));
        }

        // Sort to guarantee a stable iteration order.
        instances.sort();

        let (instances, mut next_instance) =
            if let Some((next_component, next_instance)) = next_entry {
                // Skip to the next entry. If the exact component is found, start there.
                // Otherwise start at the next component and clear any assumptions about
                // the next instance within that component.
                match instances.binary_search_by(|i| i.as_str().cmp(next_component)) {
                    Ok(idx) => (&instances[idx..], Some(next_instance)),
                    Err(idx) => (&instances[idx..], None),
                }
            } else {
                (&instances[0..], None)
            };

        for instance in instances {
            if let Ok(source) = self.provider.route_instance(&instance).await {
                let (proxy, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .map_err(|_| zx::Status::INTERNAL)?;
                if let Ok(()) = open_capability_at_source(OpenRequest {
                    flags: fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::DIRECTORY,
                    relative_path: PathBuf::new(),
                    source,
                    target: &target,
                    server_chan: &mut server.into_channel(),
                })
                .await
                {
                    if let Ok(mut dirents) = fuchsia_fs::directory::readdir(&proxy).await {
                        // Sort to guarantee a stable iteration order.
                        dirents.sort();

                        let dirents = if let Some(next_instance) = next_instance.take() {
                            // Skip to the next entry. If the exact instance is found, start there.
                            // Otherwise start at the next instance, assuming the missing one was removed.
                            match dirents.binary_search_by(|e| e.name.as_str().cmp(next_instance)) {
                                Ok(idx) | Err(idx) => &dirents[idx..],
                            }
                        } else {
                            &dirents[0..]
                        };

                        for dirent in dirents {
                            // Encode the (component,instance) tuple so that it can be represented in a single
                            // path segment. If the component and instance name are identical ignore comma separation.
                            // TODO(fxbug.dev/100985) Remove this entry name parsing scheme. Supporting component instance name
                            // prefixes is no longer necessary.
                            let entry_name = {
                                if instance == &dirent.name {
                                    instance.clone()
                                } else {
                                    format!("{},{}", &instance, &dirent.name)
                                }
                            };
                            sink = match sink.append(
                                &EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory),
                                &entry_name,
                            ) {
                                dirents_sink::AppendResult::Ok(sink) => sink,
                                dirents_sink::AppendResult::Sealed(sealed) => {
                                    // There is not enough space to return this entry. Record it as the next
                                    // entry to start at for subsequent calls.
                                    return Ok((TraversalPosition::Name(entry_name), sealed));
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok((TraversalPosition::End, sink.seal()))
    }
}

/// Represents a routed service capability from a collection in the source component.
#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub struct CollectionServiceRoute {
    /// Moniker of the component that contains the collection.
    pub source_moniker: AbsoluteMoniker,

    /// Name of the collection from which the service is exposed.
    pub collection_name: String,

    /// Name of the service exposed from the collection.
    pub service_name: CapabilityName,
}

impl CollectionServiceRoute {
    /// Returns true if the component with `moniker` is a child of the collection in this route.
    fn matches_child_component(&self, moniker: &AbsoluteMoniker) -> bool {
        let component_parent_moniker = match moniker.parent() {
            Some(moniker) => moniker,
            None => {
                // Component is the root component, and so cannot be in a collection.
                return false;
            }
        };

        let component_leaf_moniker = match moniker.leaf() {
            Some(moniker) => moniker,
            None => {
                // Component is the root component, and so cannot be in a collection.
                return false;
            }
        };

        self.source_moniker == component_parent_moniker
            && component_leaf_moniker
                .collection
                .as_ref()
                .map_or(false, |collection| *collection == self.collection_name)
    }

    /// Returns true if the component exposes the same services aggregated in this route.
    fn matches_exposed_service(&self, decl: &ComponentDecl) -> bool {
        decl.exposes.iter().any(|expose| {
            matches!(expose, ExposeDecl::Service(_)) && expose.target_name() == &self.service_name
        })
    }
}

struct CollectionServiceDirectoryInner {
    /// Directory that contains all aggregated service instances.
    pub dir: Arc<SimpleImmutableDir>,

    /// Directory entries in `dir`.
    ///
    /// This is used to find directory entries after they have been inserted into `dir`,
    /// as `dir` does not directly expose its entries.
    entries: Vec<Arc<ServiceInstanceDirectoryEntry>>,
}

pub struct CollectionServiceDirectory {
    /// The parent component of the collection and aggregated service.
    parent: WeakComponentInstance,

    /// The route for the service capability backed by this directory.
    route: CollectionServiceRoute,

    /// The provider of service capabilities for the collection being aggregated.
    ///
    /// This returns routed `CapabilitySourceInterface`s to a service capability for a
    /// component instance in the collection.
    aggregate_capability_provider: Box<dyn AggregateCapabilityProvider<ComponentInstance>>,

    inner: Mutex<CollectionServiceDirectoryInner>,
}

impl CollectionServiceDirectory {
    pub fn new(
        parent: WeakComponentInstance,
        route: CollectionServiceRoute,
        aggregate_capability_provider: Box<dyn AggregateCapabilityProvider<ComponentInstance>>,
    ) -> Self {
        CollectionServiceDirectory {
            parent,
            route,
            aggregate_capability_provider,
            inner: Mutex::new(CollectionServiceDirectoryInner {
                dir: simple_immutable_dir(),
                entries: Vec::new(),
            }),
        }
    }

    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        vec![HooksRegistration::new(
            "CollectionServiceDirectory",
            vec![EventType::CapabilityRouted, EventType::Started, EventType::Stopped],
            Arc::downgrade(self) as Weak<dyn Hook>,
        )]
    }

    /// Returns a DirectoryEntry that represents this service directory.
    pub async fn dir_entry(&self) -> Arc<dyn DirectoryEntry> {
        self.inner.lock().await.dir.clone()
    }

    /// Adds directory entries from services exposed by a child in the aggregated collection.
    pub async fn add_entries_from_child(&self, child_name: &str) -> Result<(), ModelError> {
        let parent =
            self.parent.upgrade().map_err(|err| ModelError::ComponentInstanceError { err })?;
        match self.aggregate_capability_provider.route_instance(&child_name).await {
            Ok(source) => {
                // Add entries for the component `instance`, from its `source`,
                // the service exposed by the component.
                if let Err(err) = self.add_entries_from_capability_source(&child_name, source).await
                {
                    parent
                        .with_logger_as_default(|| {
                            error!(
                                component=%child_name,
                                collection=%self.route.collection_name,
                                service_name=%self.route.service_name,
                                error=%err,
                                "Failed to add service entries from component, skipping",
                            );
                        })
                        .await;
                }
            }
            Err(err) => {
                parent
                    .with_logger_as_default(|| {
                        error!(
                            component=%child_name,
                            collection=%self.route.collection_name,
                            service_name=%self.route.service_name,
                            error=%err,
                            "Failed to route service capability from component, skipping",
                        );
                    })
                    .await;
            }
        }
        Ok(())
    }

    async fn add_entries_from_capability_source_lazy(
        self: Arc<Self>,
        component_name: String,
        source: CapabilitySource,
    ) -> Result<(), ModelError> {
        let task_scope = self.parent.upgrade()?.nonblocking_task_scope();
        // Lazily add entries for instances from the service exposed by this component.
        // This has to happen after this function, the Started hook handler, returns because
        // `add_entries_from_capability_source` reads from the exposed service directory
        // to enumerates service instances, but this directory is not served until the
        // component is actually running. The component is only started *after* all hooks run.
        let add_instances_to_dir = async move {
            self.add_entries_from_capability_source(component_name.as_str(), source)
                .then(|result| async {
                    if let Err(e) = result {
                        error!(error = ?e, "failed to add service instances");
                    }
                })
                .await;
        };
        task_scope.add_task(add_instances_to_dir).await;
        Ok(())
    }

    /// Opens the service capability at `source` and adds entries for each service instance.
    ///
    /// Entry names are named by joining the originating component, `component_name`,
    /// and the service instance name, with a comma.
    ///
    /// # Errors
    /// Returns an error if `source` is not a service capability, or could not be opened.
    pub async fn add_entries_from_capability_source(
        &self,
        component_name: &str,
        source: CapabilitySource,
    ) -> Result<(), ModelError> {
        let mut inner = self.inner.lock().await;

        // The CapabilitySource must be for a service capability.
        if source.type_name() != CapabilityTypeName::Service {
            return Err(ModelError::RoutingError {
                err: RoutingError::unsupported_capability_type(source.type_name()),
            });
        }

        let target =
            self.parent.upgrade().map_err(|err| ModelError::ComponentInstanceError { err })?;

        let (proxy, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();

        open_capability_at_source(OpenRequest {
            flags: fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            relative_path: PathBuf::new(),
            source: source.clone(),
            target: &target,
            server_chan: &mut server.into_channel(),
        })
        .on_timeout(OPEN_SERVICE_TIMEOUT.after_now(), || {
            Err(ModelError::timeout(OPEN_SERVICE_TIMEOUT))
        })
        .await?;
        let dirents = fuchsia_fs::directory::readdir(&proxy).await.map_err(|e| {
            error!("Error reading entries from service directory for component '{}', capability name '{}'. Error: {}", target.abs_moniker.clone(), source.source_name().unwrap_or(&"".into()), e);
            ModelError::open_directory_error(target.abs_moniker.clone(), component_name)
        })?;
        for dirent in dirents {
            let name = format!("{},{}", &component_name, &dirent.name);
            let entry: Arc<ServiceInstanceDirectoryEntry> =
                Arc::new(ServiceInstanceDirectoryEntry {
                    name: name.clone(),
                    source: source.clone(),
                    source_component_name: component_name.to_string(),
                    service_instance: dirent.name.to_string(),
                    parent: self.parent.clone(),
                });
            inner.dir.add_node(&name, entry.clone()).map_err(|err| {
                ModelError::CollectionServiceDirError { moniker: target.abs_moniker.clone(), err }
            })?;
            inner.entries.push(entry);
        }
        Ok(())
    }

    /// Adds directory entries from services exposed by all children in the aggregated collection.
    pub fn add_entries_from_children<'a>(self: &'a Self) -> BoxFuture<'a, Result<(), ModelError>> {
        // Return a boxed future here because this function can be called from routing::get_default_provider
        // which creates a recursive loop when initializing the capability provider for collection sourced
        // services.
        Box::pin(async move {
            join_all(self.aggregate_capability_provider.list_instances().await?.iter().map(
                |child_name| async move {
                    self.add_entries_from_child(child_name).await.map_err(|e| {
                        error!(error=%e, "error adding entries from child '{}'", child_name);
                        e
                    })
                },
            ))
            .await;
            Ok(())
        })
    }

    async fn on_started_async(
        self: Arc<Self>,
        component_moniker: &AbsoluteMoniker,
        component_decl: &ComponentDecl,
    ) -> Result<(), ModelError> {
        // If this component is a child in a collection from which the aggregated service
        // is routed, add service instances from the component's service to the aggregated service.
        if self.route.matches_child_component(component_moniker)
            && self.route.matches_exposed_service(component_decl)
        {
            let instance_name = component_moniker
                .leaf()
                .unwrap() // checked in `matches_child_component`
                .name
                .clone();
            let capability_source =
                self.aggregate_capability_provider.route_instance(instance_name.as_str()).await?;

            self.add_entries_from_capability_source_lazy(
                instance_name.to_string(),
                capability_source,
            )
            .await?;
        }
        Ok(())
    }

    async fn on_stopped_async(&self, target_moniker: &AbsoluteMoniker) -> Result<(), ModelError> {
        // If this component is a child in a collection from which the aggregated service
        // is routed, remove any of its service instances from the aggregated service.
        if self.route.matches_child_component(&target_moniker) {
            let target_name = &target_moniker
                .leaf()
                .unwrap() // checked in `matches_child_component`
                .name
                .as_str();
            let mut inner = self.inner.lock().await;
            for entry in &inner.entries {
                if entry.source_component_name == *target_name {
                    inner.dir.remove_node(&entry.name).map_err(|err| {
                        ModelError::CollectionServiceDirError {
                            moniker: target_moniker.clone(),
                            err,
                        }
                    })?;
                }
            }
            inner.entries.retain(|entry| entry.source_component_name != *target_name);
        }
        Ok(())
    }
}

#[async_trait]
impl Hook for CollectionServiceDirectory {
    async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
        match &event.payload {
            EventPayload::Started { component_decl, .. } => {
                if let ExtendedMoniker::ComponentInstance(component_moniker) = &event.target_moniker
                {
                    self.on_started_async(&component_moniker, component_decl).await?;
                }
            }
            EventPayload::Stopped { .. } => {
                let target_moniker = event
                    .target_moniker
                    .unwrap_instance_moniker_or(ModelError::UnexpectedComponentManagerMoniker)?;
                self.on_stopped_async(target_moniker).await?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// A directory entry representing an instance of a service.
/// Upon opening, performs capability routing and opens the instance at its source.
struct ServiceInstanceDirectoryEntry {
    /// The name of the entry in its parent directory.
    name: String,
    /// The source of the service capability instance to route.
    source: CapabilitySource,
    /// The name of the component that serves `source`.
    source_component_name: String,
    /// The name of the service instance directory to open at the source.
    service_instance: String,
    /// The component that is hosting the directory.
    parent: WeakComponentInstance,
}

impl DirectoryEntry for ServiceInstanceDirectoryEntry {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: vfs::path::Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let mut server_end = server_end.into_channel();
        scope.spawn(async move {
            let parent = match self.parent.upgrade() {
                Ok(parent) => parent,
                Err(_) => {
                    warn!(moniker=%self.parent.abs_moniker, "parent component of aggregated service directory is gone");
                    return;
                }
            };

            let mut relative_path = PathBuf::from(&self.service_instance);

            // Path::join with an empty string adds a trailing slash, which some VFS implementations don't like.
            if !path.is_empty() {
                relative_path = relative_path.join(path.into_string());
            }

            if let Err(err) = open_capability_at_source(OpenRequest {
                flags,
                relative_path,
                source: self.source.clone(),
                target: &parent,
                server_chan: &mut server_end,
            })
            .await
            {
                server_end
                    .close_with_epitaph(err.as_zx_status())
                    .unwrap_or_else(|error| warn!(%error, "failed to close server end"));

                parent
                    .with_logger_as_default(|| {
                        error!(
                            service_instance=%self.service_instance,
                            source_component=%self.source_component_name,
                            error=%err,
                            "Failed to open service instance from component",
                        );
                    })
                    .await;
            }
        });
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }
}

#[cfg(test)]
mod tests {

    use {
        super::*,
        crate::{
            capability::CapabilitySource,
            model::{
                component::StartReason,
                routing::providers::DirectoryEntryCapabilityProvider,
                testing::routing_test_helpers::{RoutingTest, RoutingTestBuilder},
            },
        },
        ::routing::{
            capability_source::{AggregateCapabilityProvider, ComponentCapability},
            component_instance::ComponentInstanceInterface,
            error::RoutingError,
        },
        assert_matches::assert_matches,
        cm_rust::*,
        cm_rust_testing::{ChildDeclBuilder, CollectionDeclBuilder, ComponentDeclBuilder},
        fuchsia_async as fasync,
        futures::StreamExt,
        moniker::{AbsoluteMoniker, AbsoluteMonikerBase, ChildMoniker},
        std::{
            collections::{HashMap, HashSet},
            convert::TryInto,
        },
        vfs::pseudo_directory,
    };

    #[derive(Clone)]
    struct MockAggregateCapabilityProvider {
        instances: HashMap<String, WeakComponentInstance>,
    }

    #[async_trait]
    impl AggregateCapabilityProvider<ComponentInstance> for MockAggregateCapabilityProvider {
        async fn route_instance(&self, instance: &str) -> Result<CapabilitySource, RoutingError> {
            let child_moniker = ChildMoniker::try_new(instance, None)?;
            Ok(CapabilitySource::Component {
                capability: ComponentCapability::Service(ServiceDecl {
                    name: "my.service.Service".into(),
                    source_path: Some("/svc/my.service.Service".try_into().unwrap()),
                }),
                component: self
                    .instances
                    .get(instance)
                    .ok_or_else(|| RoutingError::OfferFromChildInstanceNotFound {
                        capability_id: "my.service.Service".to_string(),
                        child_moniker,
                        moniker: AbsoluteMoniker::root(),
                    })?
                    .clone(),
            })
        }

        async fn list_instances(&self) -> Result<Vec<String>, RoutingError> {
            Ok(self.instances.keys().cloned().collect())
        }

        fn clone_boxed(&self) -> Box<dyn AggregateCapabilityProvider<ComponentInstance>> {
            Box::new(self.clone())
        }
    }

    #[derive(Clone)]
    struct DynamicMockAggregateCapabilityProvider {
        pub root_component: Arc<ComponentInstance>,
        pub collection_name: String,
    }

    impl DynamicMockAggregateCapabilityProvider {
        async fn get_instances(
            &self,
        ) -> Result<HashMap<String, WeakComponentInstance>, RoutingError> {
            let instance_map = self
                .root_component
                .lock_resolved_state()
                .await
                .map_err(|e| RoutingError::CapabilityFromComponentManagerNotFound {
                    capability_id: format!(
                        "MockAggregateCapabilityProvider error getting resolved state {}",
                        e
                    ),
                })?
                .children_in_collection(self.collection_name.as_str())
                .iter()
                .map(|(child_moniker, c)| (child_moniker.to_string(), c.as_weak()))
                .collect::<HashMap<String, WeakComponentInstance>>();

            Ok(instance_map)
        }
    }

    #[async_trait]
    impl AggregateCapabilityProvider<ComponentInstance> for DynamicMockAggregateCapabilityProvider {
        async fn route_instance(&self, instance: &str) -> Result<CapabilitySource, RoutingError> {
            let child_moniker = ChildMoniker::try_new(instance, None)?;
            Ok(CapabilitySource::Component {
                capability: ComponentCapability::Service(ServiceDecl {
                    name: "my.service.Service".into(),
                    source_path: Some("/svc/my.service.Service".try_into().unwrap()),
                }),
                component: self
                    .get_instances()
                    .await?
                    .get(instance)
                    .ok_or_else(|| RoutingError::OfferFromChildInstanceNotFound {
                        capability_id: "my.service.Service".to_string(),
                        child_moniker,
                        moniker: AbsoluteMoniker::root(),
                    })?
                    .clone(),
            })
        }

        async fn list_instances(&self) -> Result<Vec<String>, RoutingError> {
            Ok(self.get_instances().await?.keys().cloned().collect())
        }

        fn clone_boxed(&self) -> Box<dyn AggregateCapabilityProvider<ComponentInstance>> {
            Box::new(self.clone())
        }
    }

    fn open_dir(
        execution_scope: ExecutionScope,
        dir: Arc<dyn DirectoryEntry>,
    ) -> fio::DirectoryProxy {
        let (dir_proxy, server_end) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();

        dir.open(
            execution_scope,
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            vfs::path::Path::dot(),
            ServerEnd::new(server_end.into_channel()),
        );

        dir_proxy
    }

    fn create_test_component_decls() -> Vec<(&'static str, ComponentDecl)> {
        vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .use_(UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Framework,
                        source_name: "fuchsia.component.Realm".into(),
                        target_path: "/svc/fuchsia.component.Realm".try_into().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Collection("coll".to_string()),
                        source_name: "my.service.Service".into(),
                        target_name: "my.service.Service".into(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .add_collection(CollectionDeclBuilder::new_transient_collection("coll"))
                    .build(),
            ),
            (
                "foo",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "my.service.Service".into(),
                        target_name: "my.service.Service".into(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(ServiceDecl {
                        name: "my.service.Service".into(),
                        source_path: Some("/svc/my.service.Service".try_into().unwrap()),
                    })
                    .build(),
            ),
            (
                "bar",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "my.service.Service".into(),
                        target_name: "my.service.Service".into(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(ServiceDecl {
                        name: "my.service.Service".into(),
                        source_path: Some("/svc/my.service.Service".try_into().unwrap()),
                    })
                    .build(),
            ),
            (
                "baz",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "my.service.Service".into(),
                        target_name: "my.service.Service".into(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(ServiceDecl {
                        name: "my.service.Service".into(),
                        source_path: Some("/svc/my.service.Service".try_into().unwrap()),
                    })
                    .build(),
            ),
        ]
    }

    async fn wait_for_dir_content_change(
        dir_proxy: &fio::DirectoryProxy,
        original_entries: Vec<fuchsia_fs::directory::DirEntry>,
    ) -> Vec<fuchsia_fs::directory::DirEntry> {
        loop {
            // TODO(fxbug.dev/4776): Once component manager supports watching for
            // service instances, this loop should be replaced by a watcher.
            let updated_entries = fuchsia_fs::directory::readdir(dir_proxy)
                .await
                .expect("failed to read directory entries");
            if original_entries.len() != updated_entries.len() {
                return updated_entries;
            }
            fasync::Timer::new(std::time::Duration::from_millis(100)).await;
        }
    }
    async fn create_collection_service_test_realm(
        init_service_dir: bool,
    ) -> (RoutingTest, Arc<CollectionServiceDirectory>) {
        let components = create_test_component_decls();

        let mock_single_instance = pseudo_directory! {
            "default" => pseudo_directory! {
                "member" => pseudo_directory! {}
            }
        };
        let mock_dual_instance = pseudo_directory! {
            "default" => pseudo_directory! {
                "member" => pseudo_directory! {}
            },
            "secondary" => pseudo_directory! {
                "member" => pseudo_directory! {},
            }
        };

        let test = RoutingTestBuilder::new("root", components)
            .add_outgoing_path(
                "foo",
                "/svc/my.service.Service".try_into().unwrap(),
                mock_single_instance.clone(),
            )
            .add_outgoing_path(
                "bar",
                "/svc/my.service.Service".try_into().unwrap(),
                mock_single_instance,
            )
            .add_outgoing_path(
                "baz",
                "/svc/my.service.Service".try_into().unwrap(),
                mock_dual_instance,
            )
            .build()
            .await;

        test.create_dynamic_child(
            AbsoluteMoniker::root(),
            "coll",
            ChildDeclBuilder::new_lazy_child("foo"),
        )
        .await;
        test.create_dynamic_child(
            AbsoluteMoniker::root(),
            "coll",
            ChildDeclBuilder::new_lazy_child("bar"),
        )
        .await;
        test.create_dynamic_child(
            AbsoluteMoniker::root(),
            "coll",
            ChildDeclBuilder::new_lazy_child("baz"),
        )
        .await;
        let foo_component = test
            .model
            .look_up(&vec!["coll:foo"].try_into().unwrap())
            .await
            .expect("failed to find foo instance");
        let bar_component = test
            .model
            .look_up(&vec!["coll:bar"].try_into().unwrap())
            .await
            .expect("failed to find bar instance");
        let baz_component = test
            .model
            .look_up(&vec!["coll:baz"].try_into().unwrap())
            .await
            .expect("failed to find baz instance");

        let provider = MockAggregateCapabilityProvider {
            instances: {
                let mut instances = HashMap::new();
                instances.insert("foo".to_string(), foo_component.as_weak());
                instances.insert("bar".to_string(), bar_component.as_weak());
                instances.insert("baz".to_string(), baz_component.as_weak());
                instances
            },
        };

        let route = CollectionServiceRoute {
            source_moniker: AbsoluteMoniker::root(),
            collection_name: "coll".to_string(),
            service_name: "my.service.Service".into(),
        };

        let dir =
            CollectionServiceDirectory::new(test.model.root().as_weak(), route, Box::new(provider));

        if init_service_dir {
            dir.add_entries_from_children().await.expect("failed to add entries");
        }

        let dir_arc = Arc::new(dir);
        test.model.root().hooks.install(dir_arc.hooks()).await;
        (test, dir_arc)
    }

    #[fuchsia::test]
    async fn test_collection_service_directory() {
        let (test, dir_arc) = create_collection_service_test_realm(true).await;
        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir_arc.dir_entry().await);

        // List the entries of the directory served by `open`.
        let entries = fuchsia_fs::directory::readdir(&dir_proxy)
            .await
            .expect("failed to read directory entries");
        let instance_names: HashSet<String> = entries.into_iter().map(|d| d.name).collect();
        assert_eq!(instance_names.len(), 4);

        // Open one of the entries.
        let collection_dir = fuchsia_fs::directory::open_directory(
            &dir_proxy,
            instance_names.iter().next().expect("failed to get instance name"),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        )
        .await
        .expect("failed to open collection dir");

        // Make sure we're reading the expected directory.
        let entries = fuchsia_fs::directory::readdir(&collection_dir)
            .await
            .expect("failed to read instances of collection dir");
        assert!(entries.iter().find(|d| d.name == "member").is_some());

        let baz_component = test
            .model
            .look_up(&vec!["coll:baz"].try_into().unwrap())
            .await
            .expect("failed to find baz instance");
        // Test that removal of instances works
        baz_component.stop().await.expect("failed to shutdown component");
        let updated_entries = wait_for_dir_content_change(&dir_proxy, entries).await;
        assert_eq!(updated_entries.len(), 2);

        test.start_instance_and_wait_start(baz_component.abs_moniker())
            .await
            .expect("component should start");

        let updated_entries = wait_for_dir_content_change(&dir_proxy, updated_entries).await;
        assert_eq!(updated_entries.len(), 4);
    }

    #[fuchsia::test]
    async fn test_collection_service_directory_component_started() {
        let (test, dir_arc) = create_collection_service_test_realm(false).await;

        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir_arc.dir_entry().await);

        let entries = fuchsia_fs::directory::readdir(&dir_proxy)
            .await
            .expect("failed to read directory entries");
        let instance_names: HashSet<String> = entries.iter().map(|d| d.name.clone()).collect();
        // should be no entries in a non initialized collection service dir.
        assert_eq!(instance_names.len(), 0);

        let baz_component = test
            .model
            .look_up(&vec!["coll:baz"].try_into().unwrap())
            .await
            .expect("failed to find baz instance");

        // Test that starting an instance results in the collection service directory adding the relevant instances.
        baz_component.start(&StartReason::Eager).await.expect("failed to shutdown component");
        let updated_entries = wait_for_dir_content_change(&dir_proxy, entries).await;
        assert_eq!(updated_entries.len(), 2);
    }
    #[fuchsia::test]
    async fn test_collection_service_directory_component_stopped() {
        let (test, dir_arc) = create_collection_service_test_realm(true).await;

        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir_arc.dir_entry().await);

        // List the entries of the directory served by `open`.
        let entries = fuchsia_fs::directory::readdir(&dir_proxy)
            .await
            .expect("failed to read directory entries");
        let instance_names: HashSet<String> = entries.iter().map(|d| d.name.clone()).collect();
        assert_eq!(instance_names.len(), 4);

        let baz_component = test
            .model
            .look_up(&vec!["coll:baz"].try_into().unwrap())
            .await
            .expect("failed to find baz instance");

        // Test that removal of instances works
        baz_component.stop().await.expect("failed to shutdown component");
        let updated_entries = wait_for_dir_content_change(&dir_proxy, entries).await;
        assert_eq!(updated_entries.len(), 2);
    }

    #[fuchsia::test]
    async fn test_collection_service_directory_failed_to_route_child() {
        let components = create_test_component_decls();

        let mock_single_instance = pseudo_directory! {
            "default" => pseudo_directory! {
                "member" => pseudo_directory! {}
            }
        };

        let test = RoutingTestBuilder::new("root", components)
            .add_outgoing_path(
                "foo",
                "/svc/my.service.Service".try_into().unwrap(),
                mock_single_instance.clone(),
            )
            .add_outgoing_path(
                "bar",
                "/svc/my.service.Service".try_into().unwrap(),
                mock_single_instance,
            )
            .build()
            .await;

        test.create_dynamic_child(
            AbsoluteMoniker::root(),
            "coll",
            ChildDeclBuilder::new_lazy_child("foo"),
        )
        .await;
        test.create_dynamic_child(
            AbsoluteMoniker::root(),
            "coll",
            ChildDeclBuilder::new_lazy_child("bar"),
        )
        .await;
        let foo_component = test
            .model
            .look_up(&vec!["coll:foo"].try_into().unwrap())
            .await
            .expect("failed to find foo instance");

        let provider = MockAggregateCapabilityProvider {
            instances: {
                let mut instances = HashMap::new();
                instances.insert("foo".to_string(), foo_component.as_weak());
                // "bar" not added to induce a routing failure on route_instance
                instances
            },
        };

        let route = CollectionServiceRoute {
            source_moniker: AbsoluteMoniker::root(),
            collection_name: "coll".to_string(),
            service_name: "my.service.Service".into(),
        };

        let dir =
            CollectionServiceDirectory::new(test.model.root().as_weak(), route, Box::new(provider));

        dir.add_entries_from_children().await.expect("failed to add entries");
        // Entries from foo should be available even though we can't route to bar
        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir.dir_entry().await);

        // List the entries of the directory served by `open`.
        let entries = fuchsia_fs::directory::readdir(&dir_proxy)
            .await
            .expect("failed to read directory entries");
        let instance_names: HashSet<String> = entries.into_iter().map(|d| d.name).collect();
        assert_eq!(instance_names.len(), 1);
        assert_eq!(instance_names.iter().next().unwrap(), "foo,default");
    }

    #[fuchsia::test]
    async fn test_collection_readdir() {
        let components = create_test_component_decls();

        let mock_instance_foo = pseudo_directory! {
            "default" => pseudo_directory! {}
        };

        let mock_instance_bar = pseudo_directory! {
            "default" => pseudo_directory! {},
            "one" => pseudo_directory! {},
        };

        let test = RoutingTestBuilder::new("root", components)
            .add_outgoing_path(
                "foo",
                "/svc/my.service.Service".try_into().unwrap(),
                mock_instance_foo,
            )
            .add_outgoing_path(
                "bar",
                "/svc/my.service.Service".try_into().unwrap(),
                mock_instance_bar,
            )
            .build()
            .await;

        test.create_dynamic_child(
            AbsoluteMoniker::root(),
            "coll",
            ChildDeclBuilder::new_lazy_child("foo"),
        )
        .await;
        test.create_dynamic_child(
            AbsoluteMoniker::root(),
            "coll",
            ChildDeclBuilder::new_lazy_child("bar"),
        )
        .await;
        let foo_component = test
            .model
            .look_up(&vec!["coll:foo"].try_into().unwrap())
            .await
            .expect("failed to find foo instance");
        let bar_component = test
            .model
            .look_up(&vec!["coll:bar"].try_into().unwrap())
            .await
            .expect("failed to find bar instance");

        let provider = MockAggregateCapabilityProvider {
            instances: {
                let mut instances = HashMap::new();
                instances.insert("foo".to_string(), foo_component.as_weak());
                instances.insert("bar".to_string(), bar_component.as_weak());
                instances
            },
        };

        let route = CollectionServiceRoute {
            source_moniker: AbsoluteMoniker::root(),
            collection_name: "coll".to_string(),
            service_name: "my.service.Service".into(),
        };

        let dir =
            CollectionServiceDirectory::new(test.model.root().as_weak(), route, Box::new(provider));

        dir.add_entries_from_children().await.expect("failed to add entries");

        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir.dir_entry().await);

        let entries = fuchsia_fs::directory::readdir(&dir_proxy)
            .await
            .expect("failed to read directory entries");

        let instance_names: HashSet<String> = entries.into_iter().map(|d| d.name).collect();
        assert!(instance_names.contains("bar,default"));
        assert!(instance_names.contains("bar,one"));
        assert!(instance_names.contains("foo,default"));
    }

    #[fuchsia::test]
    async fn test_filtered_service() {
        let components = create_test_component_decls();

        let mock_instance_foo = pseudo_directory! {
            "default" => pseudo_directory! {},
            "one" => pseudo_directory! {},
            "two" => pseudo_directory! {},
        };

        let test = RoutingTestBuilder::new("root", components)
            .add_outgoing_path(
                "foo",
                "/svc/my.service.Service".try_into().unwrap(),
                mock_instance_foo.clone(),
            )
            .build()
            .await;

        test.create_dynamic_child(
            AbsoluteMoniker::root(),
            "coll",
            ChildDeclBuilder::new_lazy_child("foo"),
        )
        .await;
        let foo_component = test
            .model
            .look_up(&vec!["coll:foo"].try_into().unwrap())
            .await
            .expect("failed to find foo instance");

        let execution_scope = foo_component
            .lock_resolved_state()
            .await
            .expect("failed to get execution scope")
            .execution_scope()
            .clone();
        let source_provider =
            DirectoryEntryCapabilityProvider { execution_scope, entry: mock_instance_foo.clone() };

        let (service_proxy, server_end) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let mut server_end = server_end.into_channel();

        let host = Box::new(
            FilteredServiceProvider::new(
                &test.model.root(),
                vec!["default".to_string(), "two".to_string()],
                HashMap::new(),
                Box::new(source_provider),
            )
            .await
            .expect("failed to create FilteredServiceProvider"),
        );
        let task_scope = TaskScope::new();
        host.open(
            task_scope.clone(),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            PathBuf::new(),
            &mut server_end,
        )
        .await
        .expect("failed to serve");

        // Choose a value such that there is only room for a single entry.
        const MAX_BYTES: u64 = 20;
        // Make sure expected instances are found
        for n in ["default", "two"] {
            let (status, buf) = service_proxy.read_dirents(MAX_BYTES).await.expect("read_dirents");
            assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
            let entries = fuchsia_fs::directory::parse_dir_entries(&buf);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].as_ref().expect("complete entry").name, n);
        }

        // Confirm no more entries found after allow listed instances.
        let (status, buf) = service_proxy.read_dirents(MAX_BYTES).await.expect("read_dirents");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        let entries = fuchsia_fs::directory::parse_dir_entries(&buf);
        assert_eq!(entries.len(), 0);
    }

    #[fuchsia::test]
    async fn test_filtered_service_renaming() {
        let components = create_test_component_decls();

        let mock_instance_foo = pseudo_directory! {
            "default" => pseudo_directory! {},
            "one" => pseudo_directory! {},
            "two" => pseudo_directory! {},
        };

        let test = RoutingTestBuilder::new("root", components)
            .add_outgoing_path(
                "foo",
                "/svc/my.service.Service".try_into().unwrap(),
                mock_instance_foo.clone(),
            )
            .build()
            .await;

        test.create_dynamic_child(
            AbsoluteMoniker::root(),
            "coll",
            ChildDeclBuilder::new_lazy_child("foo"),
        )
        .await;
        let foo_component = test
            .model
            .look_up(&vec!["coll:foo"].try_into().unwrap())
            .await
            .expect("failed to find foo instance");

        let execution_scope = foo_component
            .lock_resolved_state()
            .await
            .expect("failed to get execution scope")
            .execution_scope()
            .clone();
        let source_provider =
            DirectoryEntryCapabilityProvider { execution_scope, entry: mock_instance_foo.clone() };

        let (service_proxy, server_end) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let mut server_end = server_end.into_channel();

        let host = Box::new(
            FilteredServiceProvider::new(
                &test.model.root(),
                vec![],
                HashMap::from([
                    ("default".to_string(), vec!["aaaaaaa".to_string(), "bbbbbbb".to_string()]),
                    ("one".to_string(), vec!["one_a".to_string()]),
                ]),
                Box::new(source_provider),
            )
            .await
            .expect("failed to create FilteredServiceProvider"),
        );

        let task_scope = TaskScope::new();
        host.open(
            task_scope.clone(),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            PathBuf::new(),
            &mut server_end,
        )
        .await
        .expect("failed to open path in filtered service directory.");

        // Choose a value such that there is only room for a single entry.
        const MAX_BYTES: u64 = 20;
        // Make sure expected instances are found
        for n in ["aaaaaaa", "bbbbbbb", "one_a", "two"] {
            let (status, buf) = service_proxy.read_dirents(MAX_BYTES).await.expect("read_dirents");
            assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
            let entries = fuchsia_fs::directory::parse_dir_entries(&buf);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].as_ref().expect("complete entry").name, n);
        }

        // Confirm no more entries found after allow listed instances.
        let (status, buf) = service_proxy.read_dirents(MAX_BYTES).await.expect("read_dirents");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        let entries = fuchsia_fs::directory::parse_dir_entries(&buf);
        assert_eq!(entries.len(), 0);
    }

    #[fuchsia::test]
    async fn test_filtered_service_error_cases() {
        let components = create_test_component_decls();

        let mock_instance_foo = pseudo_directory! {
            "default" => pseudo_directory! {},
            "one" => pseudo_directory! {},
            "two" => pseudo_directory! {},
        };

        let test = RoutingTestBuilder::new("root", components)
            .add_outgoing_path(
                "foo",
                "/svc/my.service.Service".try_into().unwrap(),
                mock_instance_foo.clone(),
            )
            .build()
            .await;

        test.create_dynamic_child(
            AbsoluteMoniker::root(),
            "coll",
            ChildDeclBuilder::new_lazy_child("foo"),
        )
        .await;
        let foo_component = test
            .model
            .look_up(&vec!["coll:foo"].try_into().unwrap())
            .await
            .expect("failed to find foo instance");

        let execution_scope = foo_component
            .lock_resolved_state()
            .await
            .expect("failed to get execution scope")
            .execution_scope()
            .clone();
        let source_provider =
            DirectoryEntryCapabilityProvider { execution_scope, entry: mock_instance_foo.clone() };

        let (service_proxy, server_end) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let mut server_end = server_end.into_channel();

        let host = Box::new(
            FilteredServiceProvider::new(
                &test.model.root(),
                vec!["default".to_string(), "two".to_string()],
                HashMap::new(),
                Box::new(source_provider),
            )
            .await
            .expect("failed to create FilteredServiceProvider"),
        );

        let task_scope = TaskScope::new();
        // expect that opening an instance that is filtered out
        let mut path_buf = PathBuf::new();
        path_buf.push("one");
        host.open(
            task_scope.clone(),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            path_buf,
            &mut server_end,
        )
        .await
        .expect("failed to open path in filtered service directory.");
        assert_matches!(
            service_proxy.take_event_stream().next().await.unwrap(),
            Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_FOUND, .. })
        );
    }
}
