// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::CapabilityProvider;
use crate::model::component::{ComponentInstance, WeakComponentInstance, WeakExtendedInstance};
use crate::model::mutable_directory::MutableDirectory;
use crate::model::routing::{CapabilityOpenRequest, RouteSource};
use async_trait::async_trait;
use cm_rust::{CapabilityTypeName, ChildRef, ComponentDecl, ExposeDecl, ExposeDeclCommon};
use cm_types::{IterablePath, Name, RelativePath};
use cm_util::TaskGroup;
use errors::{CapabilityProviderError, ModelError, OpenError};
use fidl_fuchsia_io as fio;
use flyweights::FlyStr;
use fuchsia_async::{DurationExt, TimeoutExt};
use futures::channel::oneshot;
use futures::future::{join_all, BoxFuture};
use futures::lock::Mutex;
use futures::stream::TryStreamExt;
use hooks::{Event, EventPayload, EventType, Hook, HooksRegistration};
use log::{error, warn};
use moniker::{ExtendedMoniker, Moniker};
use router_error::Explain;
use routing::capability_source::{
    AggregateInstance, AggregateMember, CapabilitySource, ComponentSource,
    FilteredAggregateCapabilityProvider,
};
use routing::collection::{
    new_filtered_aggregate_from_capability_source, AnonymizedAggregateServiceProvider,
};
use routing::component_instance::ComponentInstanceInterface;
use routing::error::RoutingError;
use routing::legacy_router::NoopVisitor;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Weak};
use vfs::directory::entry::{
    DirectoryEntry, DirectoryEntryAsync, EntryInfo, GetEntryInfo, OpenRequest,
};
use vfs::directory::immutable::simple::{
    simple as simple_immutable_dir, Simple as SimpleImmutableDir,
};
use vfs::execution_scope::ExecutionScope;
use vfs::path::Path;
use vfs::ToObjectRequest;

/// Timeout for opening a service capability when aggregating.
const OPEN_SERVICE_TIMEOUT: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(5);

/// Serves a Service directory that allows clients to list instances resulting from an aggregation of service offers
/// and to open instances.
///
pub struct FilteredAggregateServiceProvider {
    /// The directory that contains entries for all service instances
    /// across all of the aggregated source services.
    dir: Arc<SimpleImmutableDir>,
}

impl FilteredAggregateServiceProvider {
    pub async fn new(
        parent: WeakComponentInstance,
        target: WeakComponentInstance,
        provider: Box<dyn FilteredAggregateCapabilityProvider>,
    ) -> Result<FilteredAggregateServiceProvider, ModelError> {
        let dir = FilteredAggregateServiceDir::new(parent, target, provider).await?;
        Ok(FilteredAggregateServiceProvider { dir })
    }

    pub async fn new_from_capability_source(
        source: CapabilitySource,
        target: WeakComponentInstance,
    ) -> Result<Self, ModelError> {
        let aggregation_component = match source.source_moniker() {
            ExtendedMoniker::ComponentInstance(moniker) => {
                target.upgrade()?.find_absolute(&moniker).await?
            }
            ExtendedMoniker::ComponentManager => panic!("unexpected source: {:?}", source),
        };
        let provider =
            new_filtered_aggregate_from_capability_source(source, aggregation_component.as_weak());
        Self::new(aggregation_component.as_weak(), target, provider).await
    }
}

#[async_trait]
impl CapabilityProvider for FilteredAggregateServiceProvider {
    async fn open(
        self: Box<Self>,
        _task_group: TaskGroup,
        open_request: OpenRequest<'_>,
    ) -> Result<(), CapabilityProviderError> {
        open_request
            .open_dir(self.dir.clone())
            .map_err(|e| CapabilityProviderError::VfsOpenError(e))
    }
}

/// A directory entry representing a service with multiple services as its source.
/// This directory is hosted by component_manager on behalf of the component which offered multiple sources of
/// the same service capability.
///
/// This directory can be accessed by components by opening `/svc/my.service/` in their
/// incoming namespace when they have a `use my.service` declaration in their manifest, and the
/// source of `my.service` is multiple services.
struct FilteredAggregateServiceDir {}

impl FilteredAggregateServiceDir {
    pub async fn new(
        parent: WeakComponentInstance,
        target: WeakComponentInstance,
        provider: Box<dyn FilteredAggregateCapabilityProvider>,
    ) -> Result<Arc<SimpleImmutableDir>, ModelError> {
        let futs: Vec<_> = provider
            .route_instances()
            .into_iter()
            .map(|fut| async {
                let route_data = match fut.await {
                    Ok(p) => p,
                    Err(e) => {
                        if let (Ok(parent), Ok(target)) = (parent.upgrade(), target.upgrade()) {
                            target
                                .log(
                                    log::Level::Warn,
                                    "Failed to route aggregate service instance",
                                    &[
                                        &("parent", parent.moniker.to_string().as_str()),
                                        &("e", format!("{e}").as_str()),
                                    ],
                                )
                                .await;
                        }
                        return vec![];
                    }
                };
                let capability_source = Arc::new(route_data.capability_source);
                let source_instance = match capability_source.source_moniker() {
                    ExtendedMoniker::ComponentInstance(moniker) => {
                        let Ok(parent) = parent.upgrade() else {
                            return vec![];
                        };
                        let Ok(source_instance) = parent.find_absolute(&moniker).await else {
                            return vec![];
                        };
                        WeakExtendedInstance::Component(source_instance.as_weak())
                    }
                    ExtendedMoniker::ComponentManager => {
                        let Ok(parent) = parent.upgrade() else {
                            return vec![];
                        };
                        let Ok(above_root) = parent.find_above_root() else {
                            return vec![];
                        };
                        WeakExtendedInstance::AboveRoot(Arc::downgrade(&above_root))
                    }
                };
                let entries: Vec<_> = route_data
                    .instance_filter
                    .into_iter()
                    .map(|mapping| {
                        Arc::new(ServiceInstanceDirectoryEntry::<FlyStr> {
                            name: mapping.target_name.into(),
                            capability_source: capability_source.clone(),
                            source_instance: source_instance.clone(),
                            source_id: mapping.source_name.clone().into(),
                            service_instance: mapping.source_name.into(),
                        })
                    })
                    .collect();
                entries
            })
            .collect();
        let dir = simple_immutable_dir();
        for entry in join_all(futs).await.into_iter().flatten() {
            dir.add_node(&entry.name, entry.clone()).map_err(|err| {
                ModelError::ServiceDirError { moniker: target.moniker.clone(), err }
            })?;
        }
        Ok(dir)
    }
}

/// Represents a routed service capability from an anonymized aggregate defined in a component.
#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub struct AnonymizedServiceRoute {
    /// Moniker of the component that defines the anonymized aggregate.
    pub source_moniker: Moniker,

    /// All members relative to `source_moniker` which make up the aggregate.
    pub members: Vec<AggregateMember>,

    /// Name of the service exposed from the collection.
    pub service_name: Name,
}

impl AnonymizedServiceRoute {
    /// Returns true if the component with `moniker` is a member of a collection or static child in
    /// this route.
    fn matches_child_component(&self, moniker: &Moniker) -> bool {
        let component_parent_moniker = match moniker.parent() {
            Some(moniker) => moniker,
            None => {
                // Component is the root component, and so cannot be in an aggregate.
                return false;
            }
        };

        let component_leaf_name = match moniker.leaf() {
            Some(n) => n,
            None => {
                // Component is the root component, and so cannot be in an aggregate.
                return false;
            }
        };

        if self.source_moniker != component_parent_moniker {
            return false;
        }

        if let Some(collection) = component_leaf_name.collection.as_ref() {
            self.members
                .iter()
                .any(|m| matches!(m, AggregateMember::Collection(c) if c == collection))
        } else {
            let component_leaf_name: ChildRef = component_leaf_name.clone().into();
            self.members
                .iter()
                .any(|m| matches!(m, AggregateMember::Child(c) if c == &component_leaf_name))
        }
    }

    /// Returns true if the component exposes the same services aggregated in this route.
    fn matches_exposed_service(&self, decl: &ComponentDecl) -> bool {
        decl.exposes.iter().any(|expose| {
            matches!(expose, ExposeDecl::Service(_)) && expose.target_name() == &self.service_name
        })
    }
}

enum WatcherEntry {
    /// The watcher has not reached idle yet. The inner sender option will be used by the watcher
    /// to notify when it does transition to the ReachedIdle state if one exists.
    WaitingForIdle(Option<oneshot::Sender<()>>),
    /// The watcher transitions to this state when it has seen at least one idle event.
    ReachedIdle,
}

struct AnonymizedAggregateServiceDirInner {
    /// Directory that contains all aggregated service instances.
    pub dir: Arc<SimpleImmutableDir>,

    /// Directory entries in `dir`.
    ///
    /// This is used to find directory entries after they have been inserted into `dir`,
    /// as `dir` does not directly expose its entries.
    entries: HashMap<
        ServiceInstanceDirectoryKey<AggregateInstance>,
        Arc<ServiceInstanceDirectoryEntry<AggregateInstance>>,
    >,

    /// This contains entries for directory watchers that are listening for service instances.
    /// The value is an enum to indicate the various states that the watcher can be in.
    watchers_spawned: HashMap<AggregateInstance, WatcherEntry>,
}

/// A provider of a capability from an aggregation of one or more collections and static children.
/// The instance names in the aggregate will be anonymized.
///
/// This trait type-erases the capability type, so it can be handled and hosted generically.
#[async_trait]
pub trait AnonymizedAggregateCapabilityProvider: Send + Sync {
    /// Lists the instances of the capability.
    ///
    /// The instance is an opaque identifier that is only meaningful for a subsequent
    /// call to `route_instance`.
    async fn list_instances(&self) -> Result<Vec<AggregateInstance>, RoutingError>;

    /// Route the given `instance` of the capability to its source.
    async fn route_instance(
        &self,
        instance: &AggregateInstance,
    ) -> Result<CapabilitySource, RoutingError>;
}

#[async_trait]
impl AnonymizedAggregateCapabilityProvider
    for AnonymizedAggregateServiceProvider<ComponentInstance>
{
    async fn list_instances(&self) -> Result<Vec<AggregateInstance>, RoutingError> {
        AnonymizedAggregateServiceProvider::list_instances(&self).await
    }
    async fn route_instance(
        &self,
        instance: &AggregateInstance,
    ) -> Result<CapabilitySource, RoutingError> {
        AnonymizedAggregateServiceProvider::route_instance(&self, instance, &mut NoopVisitor {})
            .await
    }
}

pub struct AnonymizedAggregateServiceDir {
    /// The parent component of the collection and aggregated service.
    parent: WeakComponentInstance,

    /// The route for the service capability backed by this directory.
    route: AnonymizedServiceRoute,

    /// The provider of service capabilities for the collection being aggregated.
    ///
    /// This returns routed `CapabilitySourceInterface`s to a service capability for a
    /// component instance in the collection.
    aggregate_capability_provider: Box<dyn AnonymizedAggregateCapabilityProvider>,

    inner: Mutex<AnonymizedAggregateServiceDirInner>,
}

impl AnonymizedAggregateServiceDir {
    pub fn new(
        parent: WeakComponentInstance,
        route: AnonymizedServiceRoute,
        aggregate_capability_provider: Box<dyn AnonymizedAggregateCapabilityProvider>,
    ) -> Self {
        AnonymizedAggregateServiceDir {
            parent,
            route,
            aggregate_capability_provider,
            inner: Mutex::new(AnonymizedAggregateServiceDirInner {
                dir: simple_immutable_dir(),
                entries: HashMap::new(),
                watchers_spawned: HashMap::new(),
            }),
        }
    }

    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        vec![HooksRegistration::new(
            "AnonymizedAggregateServiceDir",
            vec![EventType::Started, EventType::Stopped],
            Arc::downgrade(self) as Weak<dyn Hook>,
        )]
    }

    /// Returns the backing directory that represents this service directory.
    pub async fn dir_entry(&self) -> Arc<SimpleImmutableDir> {
        self.inner.lock().await.dir.clone()
    }

    /// Returns metadata about all the service instances in their original representation,
    /// useful for exposing debug info. The results are returned in no particular order.
    pub async fn entries(&self) -> Vec<Arc<ServiceInstanceDirectoryEntry<AggregateInstance>>> {
        self.inner.lock().await.entries.values().cloned().collect()
    }

    /// Adds directory entries from services exposed by a member of the aggregate.
    async fn add_entries_from_instance(
        self: &Arc<Self>,
        instance: &AggregateInstance,
    ) -> Result<(), ModelError> {
        let parent =
            self.parent.upgrade().map_err(|err| ModelError::ComponentInstanceError { err })?;
        let service_name = self.route.service_name.as_str();
        match self.aggregate_capability_provider.route_instance(instance).await {
            Ok(source) => {
                // Add entries for the component `name`, from its `source`,
                // the service exposed by the component.
                // We will use this oneshot channel to know when we have reached the idle state
                // on the directory watcher that is collecting service instances.
                let (idle_sender, idle_receiver) = oneshot::channel::<()>();

                // We don't want the inner lock to be held while doing an await on the
                // idle_receiver so we do our state checking and modification while holding the
                // lock, then return if we should do the idle_receiver wait.
                let do_wait = {
                    let mut inner = self.inner.lock().await;
                    if let Some(existing) = inner.watchers_spawned.get(instance) {
                        // We have an existing entry. This means the watcher has already been
                        // spawned.
                        match existing {
                            WatcherEntry::WaitingForIdle(None) => {
                                // This means we did not have a idle_sender when we spanwed the watcher
                                // initially in |on_started_async|, so add one now.
                                inner.watchers_spawned.insert(
                                    instance.clone(),
                                    WatcherEntry::WaitingForIdle(Some(idle_sender)),
                                );

                                // Since we put in our idle_sender, we will want to do a wait.
                                true
                            }
                            WatcherEntry::ReachedIdle => {
                                // Since the watcher has already reached idle, don't wait.
                                false
                            }
                            WatcherEntry::WaitingForIdle(Some(_)) => {
                                // This should be impossible as there is no concurrent entry into
                                // this code for the same instance.
                                unreachable!()
                            }
                        }
                    } else {
                        // We have no existing entry, so no watcher has been spawned.
                        // We will insert the entry with our idle_sender and spawn the watcher.
                        inner.watchers_spawned.insert(
                            instance.clone(),
                            WatcherEntry::WaitingForIdle(Some(idle_sender)),
                        );
                        self.spawn_instance_watcher_task(instance.clone(), source.clone())?;

                        // Since we put in our idle_sender, we will want to do a wait.
                        true
                    }
                };

                if do_wait {
                    // Waits for the watcher to reach and idle event.
                    idle_receiver.await.map_err(|err| {
                        error!(
                            component:% = instance,
                            service_name:% = service_name,
                            error:% = err;
                            "Failed to reach idle state on the service instance directory watcher.",
                        );

                        ModelError::open_directory_error(
                            parent.moniker.clone(),
                            instance.to_string(),
                        )
                    })?;
                }
            }
            Err(err) => {
                parent
                    .log(
                        log::Level::Error,
                        "Failed to route service capability from component, skipping",
                        &[
                            &("component", format!("{instance}").as_str()),
                            &("service_name", service_name),
                            &("error", format!("{err}").as_str()),
                        ],
                    )
                    .await;
            }
        }
        Ok(())
    }

    /// Spawns a new task on the parent's nonblocking_task_group to create and run a directory
    /// watcher for the service instances for the aggregate.
    fn spawn_instance_watcher_task(
        self: &Arc<Self>,
        instance: AggregateInstance,
        source: CapabilitySource,
    ) -> Result<(), ModelError> {
        let task_group = self.parent.upgrade()?.nonblocking_task_group();
        let self_clone = self.clone();
        let instance_watcher_task = async move {
            let service_name = self_clone.route.service_name.as_str();

            // The CapabilitySource must be for a service capability.
            if source.type_name() != CapabilityTypeName::Service {
                error!(
                    component:% = instance,
                    service_name:% = service_name;
                    "The CapabilitySource has an invalid type: '{}'.", source.type_name()
                );
                return;
            }

            let result = self_clone.wait_for_service_directory(&instance, &source).await;
            if let Err(err) = result {
                error!(
                    component:% = instance,
                    service_name:% = service_name,
                    error:% = err;
                    "Failed to wait_for_service_directory.",
                );
                return;
            }

            let watcher = self_clone.create_instance_watcher(&instance, &source).await;

            match watcher {
                Ok(watcher) => {
                    // This is a long running watcher that is alive until the source component
                    // removes the service directory.
                    self_clone.run_instance_watcher(watcher, &instance, source).await;
                }
                Err(err) => {
                    error!(
                        component:% = instance,
                        service_name:% = service_name,
                        error:% = err;
                        "Failed to create_instance_watcher.",
                    );
                }
            }
        };

        task_group.spawn(instance_watcher_task);
        Ok(())
    }

    /// Waits for the service directory to be present. This is done by recursively waiting for each
    /// directory in the source_path. This is a no-op if the source is not a component type.
    async fn wait_for_service_directory(
        &self,
        instance: &AggregateInstance,
        source: &CapabilitySource,
    ) -> Result<(), ModelError> {
        match source {
            CapabilitySource::Component(ComponentSource { capability, moniker }) => {
                let target = self
                    .parent
                    .upgrade()
                    .map_err(|err| ModelError::ComponentInstanceError { err })?;
                let source_component = target.find_absolute(moniker).await?;

                let mut cur_path = RelativePath::dot();
                for segment in capability.source_path().unwrap().iter_segments() {
                    let (proxy, server_end) =
                        fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
                    let flags = fio::OpenFlags::DIRECTORY;
                    let mut object_request = flags.to_object_request(server_end);
                    source_component
                        .open_outgoing(OpenRequest::new(
                            source_component.execution_scope.clone(),
                            flags,
                            cur_path.to_string().try_into().map_err(|_| ModelError::BadPath)?,
                            &mut object_request,
                        ))
                        .await?;
                    let watcher =
                        fuchsia_fs::directory::Watcher::new(&proxy).await.map_err(|err| {
                            error!(
                                component:% = instance,
                                service_name:% = self.route.service_name,
                                error:% = err;
                                "Failed to get the outgoing watcher for the path '{}'.",
                                cur_path
                            );
                            ModelError::open_directory_error(
                                target.moniker.clone(),
                                instance.to_string(),
                            )
                        })?;

                    enum StreamErrorType {
                        Found,
                        StreamError(fuchsia_fs::directory::WatcherStreamError),
                        Exit,
                    }

                    let result = watcher
                        .map_err(|e| StreamErrorType::StreamError(e))
                        .try_for_each(|entry| async move {
                            let mut inner = self.inner.lock().await;
                            if !inner.watchers_spawned.contains_key(&instance) {
                                // Our task entry doesn't exist, it is removed in
                                // |on_stopped_async|, so we can exit early.
                                return Err(StreamErrorType::Exit);
                            }

                            match entry.event {
                                fuchsia_fs::directory::WatchEvent::ADD_FILE
                                | fuchsia_fs::directory::WatchEvent::EXISTING => {
                                    let filename =
                                        entry.filename.as_path().to_str().unwrap().to_owned();
                                    if filename.as_str() != segment.as_str() {
                                        return Ok(());
                                    }

                                    // Use error to terminate the try_for_each and move on to the
                                    // next piece in the path.
                                    return Err(StreamErrorType::Found);
                                }
                                fuchsia_fs::directory::WatchEvent::IDLE => {
                                    let watcher_entry =
                                        inner.watchers_spawned.get_mut(&instance).unwrap();

                                    // Notifying the idle_sender if it exists but transition back
                                    // to waiting for idle as we are still not in the inner instance
                                    // watcher.
                                    match watcher_entry {
                                        WatcherEntry::WaitingForIdle(sender_option) => {
                                            if let Some(sender) = sender_option.take() {
                                                let _ = sender.send(());
                                            }

                                            *watcher_entry = WatcherEntry::WaitingForIdle(None);
                                        }
                                        WatcherEntry::ReachedIdle => {
                                            // Inner watcher should not be running yet. That is
                                            // where we set ReachedIdle.
                                            unreachable!();
                                        }
                                    };

                                    Ok(())
                                }
                                _ => Ok(()),
                            }
                        })
                        .await;

                    match result {
                        Err(StreamErrorType::Found) => {
                            cur_path.push(segment.clone());
                            continue;
                        }
                        Err(StreamErrorType::StreamError(err)) => {
                            error!(
                                component:% = instance,
                                service_name:% = self.route.service_name,
                                error:% = err;
                                "Watcher in wait_for_service_directory ran into read error."
                            );
                        }
                        Err(StreamErrorType::Exit) => {
                            // Early exit, no log needed.
                        }
                        Ok(()) => {
                            error!(
                                component:% = instance,
                                service_name:% = self.route.service_name;
                                "Watcher in wait_for_service_directory did not find the path piece before completing.",
                            );
                        }
                    };

                    return Err(ModelError::open_directory_error(
                        target.moniker.clone(),
                        instance.to_string(),
                    ));
                }
            }
            // NOTE: If `source` is `AnonymizedAggregate`, the service
            // directory must already exist, so there is no need to watch
            _ => {}
        };
        Ok(())
    }

    /// Opens the service capability at `source` and creates a directory_watcher on it.
    ///
    /// # Errors
    /// Returns an error if `source` is not a service capability, or could not be opened.
    async fn create_instance_watcher(
        &self,
        instance: &AggregateInstance,
        source: &CapabilitySource,
    ) -> Result<fuchsia_fs::directory::Watcher, ModelError> {
        let target =
            self.parent.upgrade().map_err(|err| ModelError::ComponentInstanceError { err })?;

        let scope = ExecutionScope::new();
        let (proxy, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        let mut object_request = fio::OpenFlags::DIRECTORY.to_object_request(server);
        CapabilityOpenRequest::new_from_route_source(
            RouteSource { source: source.clone(), relative_path: Default::default() },
            &target,
            OpenRequest::new(
                scope.clone(),
                fio::OpenFlags::DIRECTORY,
                Path::dot(),
                &mut object_request,
            ),
        )?
        .open()
        .on_timeout(OPEN_SERVICE_TIMEOUT.after_now(), || Err(OpenError::Timeout))
        .await?;

        fuchsia_fs::directory::Watcher::new(&proxy).await.map_err(|err| {
            error!(
                component:% = instance,
                service_name:% = self.route.service_name,
                error:% = err;
                "Failed to create service instance directory watcher.",
            );
            ModelError::open_directory_error(target.moniker.clone(), instance.to_string())
        })
    }

    /// Runs the directory watcher on the service directory. This will discover additions and
    /// removals of service instances through the various events. For new and existing events,
    /// an entry will be added, and for removed events the entry is removed.
    async fn run_instance_watcher(
        &self,
        watcher: fuchsia_fs::directory::Watcher,
        instance: &AggregateInstance,
        source: CapabilitySource,
    ) -> () {
        let source_arc = Arc::new(source.clone());
        let source_borrow = &source_arc;
        let result = watcher
            .map_err(|e| Some(e))
            .try_for_each(|message| async move {
                let filename = message.filename.as_path().to_str().unwrap().to_owned();
                let mut inner = self.inner.lock().await;
                if !inner.watchers_spawned.contains_key(&instance) {
                    // Our task entry doesn't exist, it is removed in |on_stopped_async|,
                    // so we can exit early.
                    return Err(None);
                }

                if message.event == fuchsia_fs::directory::WatchEvent::DELETED {
                    // Our directory was deleted, so we can exit early.
                    return Err(None);
                }

                if filename == "." {
                    // Ignore the "." file.
                    return Ok::<(), Option<fuchsia_fs::directory::WatcherStreamError>>(());
                }

                match message.event {
                    fuchsia_fs::directory::WatchEvent::ADD_FILE
                    | fuchsia_fs::directory::WatchEvent::EXISTING => {
                        let instance_key = ServiceInstanceDirectoryKey::<AggregateInstance> {
                            source_id: instance.clone(),
                            service_instance: FlyStr::new(&filename),
                        };

                        // Check for duplicate entries.
                        if inner.entries.contains_key(&instance_key) {
                            return Ok(());
                        }
                        let name = Self::generate_instance_id(&mut rand::thread_rng());
                        let source_instance = (&self
                            .parent
                            .upgrade()
                            .map_err(|_| None)?
                            .find_extended_instance(&source_borrow.source_moniker())
                            .await
                            .map_err(|_| None)?)
                            .into();
                        let entry = Arc::new(ServiceInstanceDirectoryEntry::<AggregateInstance> {
                            name: name.clone().into(),
                            capability_source: source_borrow.clone(),
                            source_instance,
                            source_id: instance_key.source_id.clone(),
                            service_instance: instance_key.service_instance.clone(),
                        });
                        let result = inner.dir.add_node(&name, entry.clone());
                        if let Err(err) = result {
                            error!(
                                component:% = instance,
                                service_name:% = self.route.service_name,
                                error:% = err;
                                "Failed to add node to inner directory.",
                            );
                        }
                        inner.entries.insert(instance_key, entry);
                        Ok(())
                    }
                    fuchsia_fs::directory::WatchEvent::REMOVE_FILE => {
                        let instance_key = ServiceInstanceDirectoryKey::<AggregateInstance> {
                            source_id: instance.clone(),
                            service_instance: FlyStr::new(&filename),
                        };

                        let removed_entry = inner.entries.remove(&instance_key);
                        match removed_entry {
                            Some(removed_entry) => {
                                let result = inner.dir.remove_node(&removed_entry.name);
                                if let Err(err) = result {
                                    error!(
                                        component:% = instance,
                                        service_name:% = self.route.service_name,
                                        error:% = err;
                                        "Failed to remove node from inner directory.",
                                    );
                                }
                            }
                            None => {}
                        };

                        Ok(())
                    }
                    fuchsia_fs::directory::WatchEvent::IDLE => {
                        let watcher_entry = inner.watchers_spawned.get_mut(&instance).unwrap();

                        // Transition the watcher to the ReachedIdle state, notifying the
                        // idle_sender if it exists.
                        match watcher_entry {
                            WatcherEntry::WaitingForIdle(sender_option) => {
                                if let Some(sender) = sender_option.take() {
                                    // Ignore the send result since we still transition to
                                    // ReachedIdle.
                                    let _send_result = sender.send(());
                                }

                                *watcher_entry = WatcherEntry::ReachedIdle;
                            }
                            WatcherEntry::ReachedIdle => {}
                        }

                        Ok(())
                    }
                    fuchsia_fs::directory::WatchEvent::DELETED => unreachable!(),
                }
            })
            .await;
        if let Err(Some(err)) = result {
            let fuchsia_fs::directory::WatcherStreamError::ChannelRead(status) = err;
            if status != zx::Status::PEER_CLOSED {
                error!(
                    component:% = instance,
                    service_name:% = self.route.service_name;
                    "Instance watcher stream closed with error {:?}.", status
                );
            }
        }
    }

    /// Adds directory entries from services exposed by all children in the aggregated collection.
    pub fn add_entries_from_children<'a>(
        self: &'a Arc<Self>,
    ) -> BoxFuture<'a, Result<(), ModelError>> {
        // Return a boxed future here because this function can be called from routing::get_default_provider
        // which creates a recursive loop when initializing the capability provider for collection sourced
        // services.
        Box::pin(async move {
            join_all(self.aggregate_capability_provider.list_instances().await?.iter().map(
                |instance| async move {
                    self.add_entries_from_instance(&instance).await.map_err(|e| {
                        error!(error:% = e, instance:% = instance; "error adding entries from instance");
                        e
                    })
                },
            ))
            .await;
            Ok(())
        })
    }

    /// Generates a 128-bit uuid as a hex string.
    fn generate_instance_id(rng: &mut impl rand::Rng) -> String {
        let mut num: [u8; 16] = [0; 16];
        rng.fill_bytes(&mut num);
        num.iter().map(|byte| format!("{:02x}", byte)).collect::<Vec<String>>().join("")
    }

    async fn on_started_async(
        self: Arc<Self>,
        component_moniker: &Moniker,
        component_decl: &ComponentDecl,
    ) -> Result<(), ModelError> {
        // If this component is a child in a collection from which the aggregated service
        // is routed, add service instances from the component's service to the aggregated service.
        if self.route.matches_child_component(component_moniker)
            && self.route.matches_exposed_service(component_decl)
        {
            let child_moniker = component_moniker.leaf().unwrap(); // checked in `matches_child_component`
            let instance = AggregateInstance::Child(child_moniker.clone());
            let capability_source =
                self.aggregate_capability_provider.route_instance(&instance).await?;

            // If we have not already spawned a watcher task we want to do that here.
            let mut inner = self.inner.lock().await;
            if !inner.watchers_spawned.contains_key(&instance) {
                // We have not spawned the watcher, so we create the entry with a None oneshot
                // sender that will eventually be replaced with one if necessary from the
                // |add_entries_from_instance| method.
                inner.watchers_spawned.insert(instance.clone(), WatcherEntry::WaitingForIdle(None));

                // Spawn the watcher.
                self.spawn_instance_watcher_task(instance, capability_source)?;
            }
        }
        Ok(())
    }

    async fn on_stopped_async(&self, target_moniker: &Moniker) -> Result<(), ModelError> {
        // If this component is a child in a collection from which the aggregated service
        // is routed, remove any of its service instances from the aggregated service.
        if self.route.matches_child_component(target_moniker) {
            let target_child_moniker = target_moniker.leaf().expect("root is impossible");
            let mut inner = self.inner.lock().await;
            for entry in inner.entries.values() {
                if matches!(&entry.source_id, AggregateInstance::Child(n) if n == target_child_moniker)
                {
                    inner.dir.remove_node(&entry.name).map_err(|err| {
                        ModelError::ServiceDirError { moniker: target_moniker.clone(), err }
                    })?;
                }
            }
            inner.entries.retain(|key, _| !matches!(&key.source_id, AggregateInstance::Child(n) if n == target_child_moniker));
            inner.watchers_spawned.remove(&AggregateInstance::Child(target_child_moniker.clone()));
        }
        Ok(())
    }
}

#[async_trait]
impl Hook for AnonymizedAggregateServiceDir {
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
pub struct ServiceInstanceDirectoryEntry<T> {
    /// The name of the entry in its parent directory.
    ///
    /// This is not a [Name] because, in the case of aggregated service instances, the instance
    /// name can be any valid [fuchsia.io] filename.
    pub name: FlyStr,

    /// The source of the service capability instance to route.
    capability_source: Arc<CapabilitySource>,

    /// The source instance referred to in `capability_source`
    source_instance: WeakExtendedInstance,

    /// An identifier that can be used to find the child component that serves the service
    /// instance.
    /// This is a generic type because it varies between aggregated directory types. For example,
    /// for aggregated offers this an instance in the source instance filter,
    /// while for aggregated collections it is the moniker of the source child.
    // TODO(https://fxbug.dev/294909269): AnonymizedAggregateServiceDir needs this, but
    // FilteredAggregateServiceDir only uses this for debug info. We could probably have
    // AnonymizedAggregateServiceDir use ServiceInstanceDirectoryKey.source_id instead, and either
    // delete this or make it debug-only.
    pub source_id: T,

    /// The name of the service instance directory to open at the source.
    pub service_instance: FlyStr,
}

/// A key that uniquely identifies a ServiceInstanceDirectoryEntry.
#[derive(Hash, PartialEq, Eq)]
struct ServiceInstanceDirectoryKey<T: Send + Sync + 'static + fmt::Display> {
    /// An identifier that can be used to find the child component that serves the service
    /// instance.
    /// This is a generic type because it varies between aggregated directory types. For example,
    /// for aggregated offers this an instance in the source instance filter,
    /// while for aggregated collections it is the moniker of the source child.
    pub source_id: T,

    /// The name of the service instance directory to open at the source.
    pub service_instance: FlyStr,
}

impl<T: Send + Sync + 'static> DirectoryEntry for ServiceInstanceDirectoryEntry<T> {
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
        request.spawn(self);
        Ok(())
    }
}

impl<T: Send + Sync + 'static> GetEntryInfo for ServiceInstanceDirectoryEntry<T> {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }
}

impl<T: Send + Sync + 'static> DirectoryEntryAsync for ServiceInstanceDirectoryEntry<T> {
    async fn open_entry_async(
        self: Arc<Self>,
        mut request: OpenRequest<'_>,
    ) -> Result<(), zx::Status> {
        let source_component = match &self.source_instance {
            WeakExtendedInstance::Component(c) => c,
            WeakExtendedInstance::AboveRoot(_) => {
                unreachable!(
                    "aggregate service directory has a capability source above root, but this is \
                     impossible"
                );
            }
        };
        let Ok(source_component) = source_component.upgrade() else {
            warn!(
                moniker:% = source_component.moniker;
                "source_component of aggregated service directory is gone"
            );
            return Err(zx::Status::NOT_FOUND);
        };
        request.prepend_path(&self.service_instance.as_str().try_into().unwrap());
        let route_source = RouteSource::new((*self.capability_source).clone());
        let cap_open_request =
            CapabilityOpenRequest::new_from_route_source(route_source, &source_component, request)
                .map_err(|e| e.as_zx_status())?;
        if let Err(err) = cap_open_request.open().await {
            source_component
                .log(
                    log::Level::Error,
                    "Failed to open service instance from component",
                    &[
                        &("service_instance", self.service_instance.as_str()),
                        &("source_instance", format!("{}", source_component.moniker).as_str()),
                        &("error", format!("{err}").as_str()),
                    ],
                )
                .await;
            Err(err.as_zx_status())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::component::StartReason;
    use crate::model::routing::service::AnonymizedAggregateServiceDir;
    use crate::model::routing::RoutingError;
    use crate::model::start::Start;
    use crate::model::testing::out_dir::OutDir;
    use crate::model::testing::routing_test_helpers::{RoutingTest, RoutingTestBuilder};
    use ::routing::capability_source::{ComponentCapability, FilteredAggregateCapabilityRouteData};
    use ::routing::component_instance::ComponentInstanceInterface;
    use cm_rust::*;
    use cm_rust_testing::*;
    use fidl::endpoints::ServerEnd;
    use fuchsia_async as fasync;
    use maplit::hashmap;
    use proptest::prelude::*;
    use rand::SeedableRng;
    use std::collections::HashSet;
    use vfs::directory::entry_container::Directory;
    use vfs::pseudo_directory;

    #[derive(Clone)]
    struct MockAnonymizedCapabilityProvider {
        /// Use an Arc<Mutex> for the instances so that we can mutate it after the Aggregate
        /// directory has been created in the test.
        instances: Arc<Mutex<HashMap<AggregateInstance, WeakComponentInstance>>>,
    }

    #[async_trait]
    impl AnonymizedAggregateCapabilityProvider for MockAnonymizedCapabilityProvider {
        async fn route_instance(
            &self,
            instance: &AggregateInstance,
        ) -> Result<CapabilitySource, RoutingError> {
            Ok(CapabilitySource::Component(ComponentSource {
                capability: ComponentCapability::Service(ServiceDecl {
                    name: "my.service.Service".parse().unwrap(),
                    source_path: Some("/svc/my.service.Service".parse().unwrap()),
                }),
                moniker: self
                    .instances
                    .lock()
                    .await
                    .get(instance)
                    .ok_or_else(|| match instance {
                        AggregateInstance::Parent => RoutingError::OfferFromParentNotFound {
                            capability_id: "my.service.Service".to_string(),
                            moniker: Moniker::root(),
                        },
                        AggregateInstance::Child(instance) => {
                            RoutingError::OfferFromChildInstanceNotFound {
                                capability_id: "my.service.Service".to_string(),
                                child_moniker: instance.clone(),
                                moniker: Moniker::root(),
                            }
                        }
                        AggregateInstance::Self_ => {
                            panic!("not expected");
                        }
                    })?
                    .moniker
                    .clone(),
            }))
        }

        async fn list_instances(&self) -> Result<Vec<AggregateInstance>, RoutingError> {
            Ok(self.instances.lock().await.keys().cloned().collect())
        }
    }

    #[derive(Clone)]
    struct MockOfferCapabilityProvider {
        component: WeakComponentInstance,
        instance_filter: Vec<NameMapping>,
    }

    #[async_trait]
    impl FilteredAggregateCapabilityProvider for MockOfferCapabilityProvider {
        fn route_instances(
            &self,
        ) -> Vec<BoxFuture<'_, Result<FilteredAggregateCapabilityRouteData, RoutingError>>>
        {
            let capability_source = CapabilitySource::Component(ComponentSource {
                capability: ComponentCapability::Service(ServiceDecl {
                    name: "my.service.Service".parse().unwrap(),
                    source_path: Some("/svc/my.service.Service".parse().unwrap()),
                }),
                moniker: self.component.moniker.clone(),
            });
            let data = FilteredAggregateCapabilityRouteData {
                capability_source,
                instance_filter: self.instance_filter.clone(),
            };
            let fut = async move { Ok(data) };
            vec![Box::pin(fut)]
        }

        fn clone_boxed(&self) -> Box<dyn FilteredAggregateCapabilityProvider> {
            Box::new(self.clone())
        }
    }

    fn open_dir(execution_scope: ExecutionScope, dir: Arc<dyn Directory>) -> fio::DirectoryProxy {
        let (dir_proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();

        dir.open(
            execution_scope,
            fio::OpenFlags::DIRECTORY,
            vfs::path::Path::dot(),
            ServerEnd::new(server_end.into_channel()),
        );

        dir_proxy
    }

    fn create_test_component_decls() -> Vec<(&'static str, ComponentDecl)> {
        let leaf_component_decl = ComponentDeclBuilder::new()
            .expose(ExposeBuilder::service().name("my.service.Service").source(ExposeSource::Self_))
            .service_default("my.service.Service")
            .build();
        vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .use_(
                        UseBuilder::protocol()
                            .source(UseSource::Framework)
                            .name("fuchsia.component.Realm"),
                    )
                    .expose(
                        ExposeBuilder::service()
                            .name("my.service.Service")
                            .source(ExposeSource::Collection("coll1".parse().unwrap())),
                    )
                    .expose(
                        ExposeBuilder::service()
                            .name("my.service.Service")
                            .source(ExposeSource::Collection("coll2".parse().unwrap())),
                    )
                    .expose(
                        ExposeBuilder::service()
                            .name("my.service.Service")
                            .source_static_child("static_a"),
                    )
                    .expose(
                        ExposeBuilder::service()
                            .name("my.service.Service")
                            .source_static_child("static_b"),
                    )
                    .collection(CollectionBuilder::new().name("coll1"))
                    .collection(CollectionBuilder::new().name("coll2"))
                    .child_default("static_a")
                    .child_default("static_b")
                    // This child is not included in the aggregate.
                    .child_default("static_c")
                    .build(),
            ),
            ("foo", leaf_component_decl.clone()),
            ("bar", leaf_component_decl.clone()),
            ("baz", leaf_component_decl.clone()),
            ("static_a", leaf_component_decl.clone()),
            ("static_b", leaf_component_decl.clone()),
            ("static_c", leaf_component_decl.clone()),
        ]
    }

    async fn wait_for_dir_content_change(
        dir_proxy: &fio::DirectoryProxy,
        original_entries: Vec<fuchsia_fs::directory::DirEntry>,
    ) -> Vec<fuchsia_fs::directory::DirEntry> {
        loop {
            // TODO(https://fxbug.dev/294909269): Now that component manager supports watching for
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

    async fn create_anonymized_service_test_realm(
        init_service_dir: bool,
    ) -> (RoutingTest, Arc<AnonymizedAggregateServiceDir>) {
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
                "/svc/my.service.Service".parse().unwrap(),
                mock_single_instance.clone(),
            )
            .add_outgoing_path(
                "bar",
                "/svc/my.service.Service".parse().unwrap(),
                mock_single_instance.clone(),
            )
            .add_outgoing_path(
                "baz",
                "/svc/my.service.Service".parse().unwrap(),
                mock_dual_instance,
            )
            .add_outgoing_path(
                "static_a",
                "/svc/my.service.Service".parse().unwrap(),
                mock_single_instance.clone(),
            )
            .add_outgoing_path(
                "static_b",
                "/svc/my.service.Service".parse().unwrap(),
                mock_single_instance.clone(),
            )
            .add_outgoing_path(
                "static_c",
                "/svc/my.service.Service".parse().unwrap(),
                mock_single_instance,
            )
            .build()
            .await;

        test.create_dynamic_child(&Moniker::root(), "coll1", ChildBuilder::new().name("foo")).await;
        test.create_dynamic_child(&Moniker::root(), "coll1", ChildBuilder::new().name("bar")).await;
        test.create_dynamic_child(&Moniker::root(), "coll2", ChildBuilder::new().name("baz")).await;
        let root = test.model.root();
        let foo_component =
            root.find_and_maybe_resolve(&"coll1:foo".parse().unwrap()).await.unwrap();
        let bar_component =
            root.find_and_maybe_resolve(&"coll1:bar".parse().unwrap()).await.unwrap();
        let baz_component =
            root.find_and_maybe_resolve(&"coll2:baz".parse().unwrap()).await.unwrap();
        let static_a_component =
            root.find_and_maybe_resolve(&"static_a".parse().unwrap()).await.unwrap();
        let static_b_component =
            root.find_and_maybe_resolve(&"static_b".parse().unwrap()).await.unwrap();

        let provider = MockAnonymizedCapabilityProvider {
            instances: Arc::new(Mutex::new(hashmap! {
                AggregateInstance::Child("coll1:foo".try_into().unwrap()) => foo_component.as_weak(),
                AggregateInstance::Child("coll1:bar".try_into().unwrap()) => bar_component.as_weak(),
                AggregateInstance::Child("coll2:baz".try_into().unwrap()) => baz_component.as_weak(),
                AggregateInstance::Child("static_a".try_into().unwrap()) => static_a_component.as_weak(),
                AggregateInstance::Child("static_b".try_into().unwrap()) => static_b_component.as_weak(),
            })),
        };

        let route = AnonymizedServiceRoute {
            source_moniker: Moniker::root(),
            members: vec![
                AggregateMember::Collection("coll1".parse().unwrap()),
                AggregateMember::Collection("coll2".parse().unwrap()),
                AggregateMember::Child(ChildRef {
                    name: "static_a".parse().unwrap(),
                    collection: None,
                }),
                AggregateMember::Child(ChildRef {
                    name: "static_b".parse().unwrap(),
                    collection: None,
                }),
            ],
            service_name: "my.service.Service".parse().unwrap(),
        };

        let dir =
            Arc::new(AnonymizedAggregateServiceDir::new(root.as_weak(), route, Box::new(provider)));

        if init_service_dir {
            dir.add_entries_from_children().await.expect("failed to add entries");
        }

        root.hooks.install(dir.hooks()).await;
        (test, dir)
    }

    #[fuchsia::test]
    async fn test_anonymized_service_directory() {
        let (test, dir_arc) = create_anonymized_service_test_realm(true).await;
        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir_arc.dir_entry().await);

        // List the entries of the directory served by `open`, and compare them to the
        // internal state.
        let instance_names = {
            let instance_names: HashSet<_> =
                dir_arc.entries().await.into_iter().map(|e| e.name.to_string()).collect();
            let dir_contents = fuchsia_fs::directory::readdir(&dir_proxy)
                .await
                .expect("failed to read directory entries");
            let dir_instance_names: HashSet<_> = dir_contents.into_iter().map(|d| d.name).collect();
            assert_eq!(instance_names.len(), 6);
            assert_eq!(dir_instance_names, instance_names);
            instance_names
        };

        // Open one of the entries.
        {
            let instance_dir = fuchsia_fs::directory::open_directory(
                &dir_proxy,
                instance_names.iter().next().expect("failed to get instance name"),
                fio::Flags::empty(),
            )
            .await
            .expect("failed to open collection dir");

            // Make sure we're reading the expected directory.
            let instance_dir_contents = fuchsia_fs::directory::readdir(&instance_dir)
                .await
                .expect("failed to read instances of collection dir");
            assert!(instance_dir_contents.iter().find(|d| d.name == "member").is_some());
        }

        let root = test.model.root();
        let baz_component =
            root.find_and_maybe_resolve(&vec!["coll2:baz"].try_into().unwrap()).await.unwrap();
        let static_a_component =
            root.find_and_maybe_resolve(&vec!["static_a"].try_into().unwrap()).await.unwrap();

        // Add entries from the children again. This should be a no-op since all of them are
        // already there and we prevent duplicates.
        let dir_contents = {
            let previous_entries: HashSet<_> = dir_arc
                .entries()
                .await
                .into_iter()
                .map(|e| (e.name.clone(), e.source_id.clone(), e.service_instance.clone()))
                .collect();
            dir_arc.add_entries_from_children().await.unwrap();
            let entries: HashSet<_> = dir_arc
                .entries()
                .await
                .into_iter()
                .map(|e| (e.name.clone(), e.source_id.clone(), e.service_instance.clone()))
                .collect();
            assert_eq!(entries, previous_entries);
            let dir_contents = fuchsia_fs::directory::readdir(&dir_proxy)
                .await
                .expect("failed to read directory entries");
            let dir_instance_names: HashSet<_> =
                dir_contents.iter().map(|d| d.name.clone()).collect();
            assert_eq!(dir_instance_names, instance_names);
            dir_contents
        };

        // Test that removal of instances works (both dynamic and static).
        {
            baz_component.stop().await.unwrap();
            let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
            assert_eq!(dir_contents.len(), 4);

            static_a_component.stop().await.unwrap();
            let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
            assert_eq!(dir_contents.len(), 3);

            test.start_instance_and_wait_start(static_a_component.moniker()).await.unwrap();
            let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
            assert_eq!(dir_contents.len(), 4);

            test.start_instance_and_wait_start(baz_component.moniker()).await.unwrap();
            let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
            assert_eq!(dir_contents.len(), 6);
        }
    }

    #[fuchsia::test]
    async fn test_anonymized_service_directory_with_dynamic_instances() {
        let components = create_test_component_decls();

        let mut foo_out_dir = OutDir::new();
        let mut bar_out_dir = OutDir::new();
        let mut static_a_out_dir = OutDir::new();

        // Start out "foo" with no entries in the svc directory.
        let foo_svc = pseudo_directory! {};
        foo_out_dir.add_entry("/svc".parse().unwrap(), foo_svc.clone());

        // Start out "bar" with 1 service instance in the svc directory.
        let bar_service = pseudo_directory! {
            "default" => pseudo_directory! {
                "member" => pseudo_directory! {}
            }
        };
        let bar_svc = pseudo_directory! {
            "my.service.Service" => bar_service.clone(),
        };
        bar_out_dir.add_entry("/svc".parse().unwrap(), bar_svc);

        // Start out "static_a" with no entries in the svc directory.
        let static_a_svc = pseudo_directory! {};
        static_a_out_dir.add_entry("/svc".parse().unwrap(), static_a_svc.clone());

        let test = RoutingTestBuilder::new("root", components)
            .set_component_outgoing_host_fn("foo", foo_out_dir.host_fn())
            .set_component_outgoing_host_fn("bar", bar_out_dir.host_fn())
            .set_component_outgoing_host_fn("static_a", static_a_out_dir.host_fn())
            .build()
            .await;

        test.create_dynamic_child(&Moniker::root(), "coll1", ChildBuilder::new().name("foo")).await;
        test.create_dynamic_child(&Moniker::root(), "coll1", ChildBuilder::new().name("bar")).await;
        let root = test.model.root();
        let foo_component =
            root.find_and_maybe_resolve(&"coll1:foo".parse().unwrap()).await.unwrap();
        let bar_component =
            root.find_and_maybe_resolve(&"coll1:bar".parse().unwrap()).await.unwrap();
        let static_a_component =
            root.find_and_maybe_resolve(&"static_a".parse().unwrap()).await.unwrap();

        let provider = MockAnonymizedCapabilityProvider {
            instances: Arc::new(Mutex::new(hashmap! {
                AggregateInstance::Child("coll1:foo".try_into().unwrap()) => foo_component.as_weak(),
                AggregateInstance::Child("coll1:bar".try_into().unwrap()) => bar_component.as_weak(),
                AggregateInstance::Child("static_a".try_into().unwrap()) => static_a_component.as_weak(),
            })),
        };

        let route = AnonymizedServiceRoute {
            source_moniker: Moniker::root(),
            members: vec![
                AggregateMember::Collection("coll1".parse().unwrap()),
                AggregateMember::Collection("coll2".parse().unwrap()),
                AggregateMember::Child(ChildRef {
                    name: "static_a".parse().unwrap(),
                    collection: None,
                }),
            ],
            service_name: "my.service.Service".parse().unwrap(),
        };

        let dir_arc = Arc::new(AnonymizedAggregateServiceDir::new(
            root.as_weak(),
            route,
            Box::new(provider.clone()),
        ));
        root.hooks.install(dir_arc.hooks()).await;
        dir_arc.add_entries_from_children().await.unwrap();

        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir_arc.dir_entry().await);

        // Ensure the instance we had initially in "bar" is there.
        let entries = dir_arc.entries().await.len();
        assert_eq!(entries, 1);

        let dir_contents = fuchsia_fs::directory::readdir(&dir_proxy)
            .await
            .expect("failed to read directory entries");
        let previous_entries = entries;

        // Add 1 instance to "foo" and ensure we can get it.
        let foo_service = pseudo_directory! {
            "default" => pseudo_directory! {
                "member" => pseudo_directory! {},
            },
        };
        foo_svc
            .add_node("my.service.Service", foo_service.clone())
            .expect("Could not add service node.");
        let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        let entries = dir_arc.entries().await.len();
        assert_eq!(entries, previous_entries + 1);
        let previous_entries = entries;

        // Add another instance (total of 2) to "foo" and ensure we can get it.
        foo_service
            .add_node(
                "secondary",
                pseudo_directory! {
                    "member" => pseudo_directory! {},
                },
            )
            .expect("Could not add service node.");
        let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        let entries = dir_arc.entries().await.len();
        assert_eq!(entries, previous_entries + 1);
        let previous_entries = entries;

        // Add another instance to "bar", which had 1 instance previously and ensure we can get it.
        bar_service
            .add_node(
                "secondary",
                pseudo_directory! {
                    "member" => pseudo_directory! {},
                },
            )
            .expect("Could not add service node.");
        let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        let entries = dir_arc.entries().await.len();
        assert_eq!(entries, previous_entries + 1);
        let previous_entries = entries;

        // Add 2 instances to the "static_a" and ensure we can get it.
        static_a_svc
            .add_node(
                "my.service.Service",
                pseudo_directory! {
                    "default" => pseudo_directory! {
                        "member" => pseudo_directory! {},
                    },
                    "secondary" => pseudo_directory! {
                        "member" => pseudo_directory! {},
                    },
                },
            )
            .expect("Could not add service node.");
        let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        if dir_contents.len() == previous_entries + 1 {
            // in case we caught the change before both were seen.
            wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        }
        let entries = dir_arc.entries().await.len();
        assert_eq!(entries, previous_entries + 2);

        // Read the directory for final check.
        // 2 in each of the 3 components.
        let dir_contents = fuchsia_fs::directory::readdir(&dir_proxy)
            .await
            .expect("failed to read directory entries");
        assert_eq!(dir_contents.len(), 6);
        for entry in &dir_contents {
            let instance_dir =
                fuchsia_fs::directory::open_directory(&dir_proxy, &entry.name, fio::Flags::empty())
                    .await
                    .expect("failed to open collection dir");

            // Make sure we're reading the expected directory.
            let instance_dir_contents = fuchsia_fs::directory::readdir(&instance_dir)
                .await
                .expect("failed to read instances of collection dir");
            assert!(instance_dir_contents.iter().find(|d| d.name == "member").is_some());
        }

        // Remove some entries to make sure removal flow works.
        bar_service.remove_node("default").expect("Failed to remove default from bar.");
        let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        assert_eq!(dir_contents.len(), 5);

        bar_service.remove_node("secondary").expect("Failed to remove secondary from bar.");
        let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        assert_eq!(dir_contents.len(), 4);
    }

    #[fuchsia::test]
    async fn test_anonymized_service_directory_with_dynamic_instances_dynamic_child() {
        let components = create_test_component_decls();
        let mut baz_out_dir = OutDir::new();
        // Setup "baz" with 1 instance.
        let baz_service = pseudo_directory! {
            "default" => pseudo_directory! {
                "member" => pseudo_directory! {}
            }
        };
        let baz_svc = pseudo_directory! {
            "my.service.Service" => baz_service.clone(),
        };
        baz_out_dir.add_entry("/svc".parse().unwrap(), baz_svc);

        let test = RoutingTestBuilder::new("root", components)
            .set_component_outgoing_host_fn("baz", baz_out_dir.host_fn())
            .build()
            .await;

        let root = test.model.root();

        let provider =
            MockAnonymizedCapabilityProvider { instances: Arc::new(Mutex::new(hashmap! {})) };

        let route = AnonymizedServiceRoute {
            source_moniker: Moniker::root(),
            members: vec![AggregateMember::Collection("coll2".parse().unwrap())],
            service_name: "my.service.Service".parse().unwrap(),
        };

        let dir_arc = Arc::new(AnonymizedAggregateServiceDir::new(
            root.as_weak(),
            route,
            Box::new(provider.clone()),
        ));
        root.hooks.install(dir_arc.hooks()).await;

        // Initialize the aggregate. There are no components yet so it will be empty.
        dir_arc.add_entries_from_children().await.unwrap();
        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir_arc.dir_entry().await);
        let entries = dir_arc.entries().await.len();
        assert_eq!(entries, 0);
        let dir_contents = fuchsia_fs::directory::readdir(&dir_proxy)
            .await
            .expect("failed to read directory entries");

        // Create and start "baz" component. Ensure the start hook added the entry since the
        // aggregate already exists.
        test.create_dynamic_child(&Moniker::root(), "coll2", ChildBuilder::new().name("baz")).await;
        let baz_component =
            root.find_and_maybe_resolve(&"coll2:baz".parse().unwrap()).await.unwrap();
        provider.instances.lock().await.insert(
            AggregateInstance::Child("coll2:baz".try_into().unwrap()),
            baz_component.as_weak(),
        );
        baz_component.ensure_started(&StartReason::Eager).await.unwrap();
        let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        let entries = dir_arc.entries().await.len();
        assert_eq!(entries, 1);
        let previous_entries = entries;

        // Add one more instance to "baz"
        baz_service
            .add_node(
                "secondary",
                pseudo_directory! {
                    "member" => pseudo_directory! {},
                },
            )
            .expect("Could not add service node.");
        wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        let entries = dir_arc.entries().await.len();
        assert_eq!(entries, previous_entries + 1);

        // Validate they both have "member".
        let dir_contents = fuchsia_fs::directory::readdir(&dir_proxy)
            .await
            .expect("failed to read directory entries");
        assert_eq!(dir_contents.len(), 2);
        for entry in &dir_contents {
            let instance_dir =
                fuchsia_fs::directory::open_directory(&dir_proxy, &entry.name, fio::Flags::empty())
                    .await
                    .expect("failed to open collection dir");

            // Make sure we're reading the expected directory.
            let instance_dir_contents = fuchsia_fs::directory::readdir(&instance_dir)
                .await
                .expect("failed to read instances of collection dir");
            assert!(instance_dir_contents.iter().find(|d| d.name == "member").is_some());
        }

        // Remove some entries to make sure removal flow works.
        baz_service.remove_node("default").expect("Failed to remove default from baz.");
        let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        assert_eq!(dir_contents.len(), 1);

        baz_service.remove_node("secondary").expect("Failed to remove secondary from baz.");
        let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        assert_eq!(dir_contents.len(), 0);
    }

    #[fuchsia::test]
    async fn test_anonymized_service_directory_with_dynamic_instances_start_before_add() {
        let components = create_test_component_decls();
        let mut static_b_out_dir = OutDir::new();

        // Setup "static_b" with 1 instance.
        let static_b_service = pseudo_directory! {
            "default" => pseudo_directory! {
                "member" => pseudo_directory! {}
            }
        };
        let static_b_svc = pseudo_directory! {
            "my.service.Service" => static_b_service.clone(),
        };
        static_b_out_dir.add_entry("/svc".parse().unwrap(), static_b_svc);

        let test = RoutingTestBuilder::new("root", components)
            .set_component_outgoing_host_fn("static_b", static_b_out_dir.host_fn())
            .build()
            .await;

        let root = test.model.root();
        let static_b_component =
            root.find_and_maybe_resolve(&"static_b".parse().unwrap()).await.unwrap();

        let provider = MockAnonymizedCapabilityProvider {
            instances: Arc::new(Mutex::new(hashmap! {
                AggregateInstance::Child("static_b".try_into().unwrap()) => static_b_component.as_weak(),
            })),
        };

        let route = AnonymizedServiceRoute {
            source_moniker: Moniker::root(),
            members: vec![AggregateMember::Child(ChildRef {
                name: "static_b".parse().unwrap(),
                collection: None,
            })],
            service_name: "my.service.Service".parse().unwrap(),
        };

        let dir_arc = Arc::new(AnonymizedAggregateServiceDir::new(
            root.as_weak(),
            route,
            Box::new(provider.clone()),
        ));
        root.hooks.install(dir_arc.hooks()).await;

        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir_arc.dir_entry().await);

        // Ensure we are starting with 0 entries.
        let dir_contents = fuchsia_fs::directory::readdir(&dir_proxy)
            .await
            .expect("failed to read directory entries");
        assert_eq!(dir_contents.len(), 0);

        // We will start "static_b" before we add_entries_from_children.
        static_b_component.ensure_started(&StartReason::Eager).await.unwrap();

        // Ensure that the start hook added the instance.
        let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        let entries = dir_arc.entries().await.len();
        assert_eq!(entries, 1);
        assert_eq!(dir_contents.len(), 1);

        // This should be a no-op.
        dir_arc.add_entries_from_children().await.unwrap();
        let dir_contents = fuchsia_fs::directory::readdir(&dir_proxy)
            .await
            .expect("failed to read directory entries");
        let entries = dir_arc.entries().await.len();
        assert_eq!(entries, 1);
        assert_eq!(dir_contents.len(), 1);

        // Add another instance to "static_b", which had 1 instance previously.
        static_b_service
            .add_node(
                "secondary",
                pseudo_directory! {
                    "member" => pseudo_directory! {},
                },
            )
            .expect("Could not add service node.");
        wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        let entries = dir_arc.entries().await.len();
        assert_eq!(entries, 2);

        // Check both.
        let dir_contents = fuchsia_fs::directory::readdir(&dir_proxy)
            .await
            .expect("failed to read directory entries");
        assert_eq!(dir_contents.len(), 2);
        for entry in &dir_contents {
            let instance_dir =
                fuchsia_fs::directory::open_directory(&dir_proxy, &entry.name, fio::Flags::empty())
                    .await
                    .expect("failed to open collection dir");

            // Make sure we're reading the expected directory.
            let instance_dir_contents = fuchsia_fs::directory::readdir(&instance_dir)
                .await
                .expect("failed to read instances of collection dir");
            assert!(instance_dir_contents.iter().find(|d| d.name == "member").is_some());
        }

        // Remove some entries to make sure removal flow works.
        static_b_service.remove_node("default").expect("Failed to remove default from bar.");
        let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        assert_eq!(dir_contents.len(), 1);

        static_b_service.remove_node("secondary").expect("Failed to remove secondary from bar.");
        let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
        assert_eq!(dir_contents.len(), 0);
    }

    #[fuchsia::test]
    async fn test_anonymized_service_directory_with_parent_and_self() {
        let leaf_component_decl = ComponentDeclBuilder::new()
            .expose(ExposeBuilder::service().name("my.service.Service").source(ExposeSource::Self_))
            .service_default("my.service.Service")
            .build();
        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .service_default("my.service.Service")
                    .offer(
                        OfferBuilder::service()
                            .name("my.service.Service")
                            .source(OfferSource::Self_)
                            .target_static_child("container")
                            .availability(cm_rust::Availability::Required),
                    )
                    .child_default("container")
                    .build(),
            ),
            (
                "container",
                ComponentDeclBuilder::new()
                    .service_default("my.service.Service")
                    .use_(
                        UseBuilder::protocol()
                            .source(UseSource::Framework)
                            .name("fuchsia.component.Realm"),
                    )
                    .offer(
                        OfferBuilder::service()
                            .name("my.service.Service")
                            .source(OfferSource::Collection("coll".parse().unwrap()))
                            .target_static_child("target")
                            .availability(cm_rust::Availability::Required),
                    )
                    .offer(
                        OfferBuilder::service()
                            .name("my.service.Service")
                            .source(OfferSource::Parent)
                            .target_static_child("target")
                            .availability(cm_rust::Availability::Required),
                    )
                    .offer(
                        OfferBuilder::service()
                            .name("my.service.Service")
                            .source(OfferSource::Self_)
                            .target_static_child("target")
                            .availability(cm_rust::Availability::Required),
                    )
                    .collection_default("coll")
                    .child_default("target")
                    .build(),
            ),
            (
                "target",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::protocol().name("my.service.Service"))
                    .build(),
            ),
            ("foo", leaf_component_decl.clone()),
            ("bar", leaf_component_decl.clone()),
        ];

        let mock_single_instance = pseudo_directory! {
            "default" => pseudo_directory! {
                "member" => pseudo_directory! {}
            }
        };

        let test = RoutingTestBuilder::new("root", components)
            .add_outgoing_path(
                "foo",
                "/svc/my.service.Service".parse().unwrap(),
                mock_single_instance.clone(),
            )
            .add_outgoing_path(
                "bar",
                "/svc/my.service.Service".parse().unwrap(),
                mock_single_instance.clone(),
            )
            .add_outgoing_path(
                "container",
                "/svc/my.service.Service".parse().unwrap(),
                mock_single_instance.clone(),
            )
            .add_outgoing_path(
                "root",
                "/svc/my.service.Service".parse().unwrap(),
                mock_single_instance,
            )
            .build()
            .await;

        test.create_dynamic_child(
            &"container".parse().unwrap(),
            "coll",
            ChildBuilder::new().name("foo"),
        )
        .await;
        test.create_dynamic_child(
            &"container".parse().unwrap(),
            "coll",
            ChildBuilder::new().name("bar"),
        )
        .await;

        let root = test.model.root();
        let container_component =
            root.find_and_maybe_resolve(&"container".parse().unwrap()).await.unwrap();
        let foo_component =
            root.find_and_maybe_resolve(&"container/coll:foo".parse().unwrap()).await.unwrap();
        let bar_component =
            root.find_and_maybe_resolve(&"container/coll:bar".parse().unwrap()).await.unwrap();

        let provider = MockAnonymizedCapabilityProvider {
            instances: Arc::new(Mutex::new(hashmap! {
                AggregateInstance::Parent => root.as_weak(),
                AggregateInstance::Self_ => container_component.as_weak(),
                AggregateInstance::Child("coll:foo".try_into().unwrap()) => foo_component.as_weak(),
                AggregateInstance::Child("coll:bar".try_into().unwrap()) => bar_component.as_weak(),
            })),
        };

        let route = AnonymizedServiceRoute {
            source_moniker: "container".parse().unwrap(),
            members: vec![
                AggregateMember::Collection("coll".parse().unwrap()),
                AggregateMember::Parent,
                AggregateMember::Self_,
            ],
            service_name: "my.service.Service".parse().unwrap(),
        };

        let dir =
            Arc::new(AnonymizedAggregateServiceDir::new(root.as_weak(), route, Box::new(provider)));
        dir.add_entries_from_children().await.unwrap();

        root.hooks.install(dir.hooks()).await;

        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir.dir_entry().await);

        // List the entries of the directory served by `open`, and compare them to the
        // internal state.
        let instance_names = {
            let instance_names: HashSet<_> =
                dir.entries().await.into_iter().map(|e| e.name.to_string()).collect();
            let dir_contents = fuchsia_fs::directory::readdir(&dir_proxy).await.unwrap();
            let dir_instance_names: HashSet<_> = dir_contents.into_iter().map(|d| d.name).collect();
            assert_eq!(instance_names.len(), 4);
            assert_eq!(dir_instance_names, instance_names);
            instance_names
        };

        // Open one of the entries.
        {
            let instance_dir = fuchsia_fs::directory::open_directory(
                &dir_proxy,
                instance_names.iter().next().unwrap(),
                fio::Flags::empty(),
            )
            .await
            .unwrap();

            // Make sure we're reading the expected directory.
            let instance_dir_contents =
                fuchsia_fs::directory::readdir(&instance_dir).await.unwrap();
            assert!(instance_dir_contents.iter().find(|d| d.name == "member").is_some());
        }

        // Add entries from the children again. This should be a no-op since all of them are
        // already there and we prevent duplicates.
        let dir_contents = {
            let previous_entries: HashSet<_> = dir
                .entries()
                .await
                .into_iter()
                .map(|e| (e.name.clone(), e.source_id.clone(), e.service_instance.clone()))
                .collect();
            dir.add_entries_from_children().await.unwrap();
            let entries: HashSet<_> = dir
                .entries()
                .await
                .into_iter()
                .map(|e| (e.name.clone(), e.source_id.clone(), e.service_instance.clone()))
                .collect();
            assert_eq!(entries, previous_entries);
            let dir_contents = fuchsia_fs::directory::readdir(&dir_proxy).await.unwrap();
            let dir_instance_names: HashSet<_> =
                dir_contents.iter().map(|d| d.name.clone()).collect();
            assert_eq!(dir_instance_names, instance_names);
            dir_contents
        };

        // Test that removal of instances works.
        {
            bar_component.stop().await.unwrap();
            let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
            assert_eq!(dir_contents.len(), 3);

            test.start_instance_and_wait_start(bar_component.moniker()).await.unwrap();
            let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
            assert_eq!(dir_contents.len(), 4);
        }
    }

    #[fuchsia::test]
    async fn test_anonymized_service_directory_component_started() {
        let (test, dir_arc) = create_anonymized_service_test_realm(false).await;

        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir_arc.dir_entry().await);

        let entries = fuchsia_fs::directory::readdir(&dir_proxy).await.unwrap();
        let instance_names: HashSet<String> = entries.iter().map(|d| d.name.clone()).collect();
        // should be no entries in a non initialized collection service dir.
        assert_eq!(instance_names.len(), 0);

        let root = test.model.root();
        let foo_component =
            root.find_and_maybe_resolve(&vec!["coll1:foo"].try_into().unwrap()).await.unwrap();

        // Test that starting an instance results in the collection service directory adding the
        // relevant instances.
        foo_component.ensure_started(&StartReason::Eager).await.unwrap();
        let entries = wait_for_dir_content_change(&dir_proxy, entries).await;
        assert_eq!(entries.len(), 1);

        let baz_component =
            root.find_and_maybe_resolve(&vec!["coll2:baz"].try_into().unwrap()).await.unwrap();

        // Test with second collection
        baz_component.ensure_started(&StartReason::Eager).await.unwrap();
        let entries = wait_for_dir_content_change(&dir_proxy, entries).await;
        assert_eq!(entries.len(), 3);

        let static_a_component =
            root.find_and_maybe_resolve(&vec!["static_a"].try_into().unwrap()).await.unwrap();

        // Test with static child
        static_a_component.ensure_started(&StartReason::Eager).await.unwrap();
        let entries = wait_for_dir_content_change(&dir_proxy, entries).await;
        assert_eq!(entries.len(), 4);
    }

    #[fuchsia::test]
    async fn test_anonymized_service_directory_component_stopped() {
        let (test, dir_arc) = create_anonymized_service_test_realm(true).await;

        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir_arc.dir_entry().await);

        // List the entries of the directory served by `open`.
        let entries = fuchsia_fs::directory::readdir(&dir_proxy).await.unwrap();
        let instance_names: HashSet<String> = entries.iter().map(|d| d.name.clone()).collect();
        assert_eq!(instance_names.len(), 6);

        let root = test.model.root();
        let foo_component =
            root.find_and_maybe_resolve(&vec!["coll1:foo"].try_into().unwrap()).await.unwrap();

        // Test that removal of instances works
        foo_component.stop().await.unwrap();
        let entries = wait_for_dir_content_change(&dir_proxy, entries).await;
        assert_eq!(entries.len(), 5);

        let baz_component =
            root.find_and_maybe_resolve(&vec!["coll2:baz"].try_into().unwrap()).await.unwrap();

        // Test with second collection
        baz_component.stop().await.unwrap();
        let entries = wait_for_dir_content_change(&dir_proxy, entries).await;
        assert_eq!(entries.len(), 3);

        let static_a_component =
            root.find_and_maybe_resolve(&vec!["static_a"].try_into().unwrap()).await.unwrap();

        // Test with static child
        static_a_component.stop().await.unwrap();
        let entries = wait_for_dir_content_change(&dir_proxy, entries).await;
        assert_eq!(entries.len(), 2);
    }

    #[fuchsia::test]
    async fn test_anonymized_service_directory_failed_to_route_child() {
        let components = create_test_component_decls();

        let mock_single_instance = pseudo_directory! {
            "default" => pseudo_directory! {
                "member" => pseudo_directory! {}
            }
        };

        let test = RoutingTestBuilder::new("root", components)
            .add_outgoing_path(
                "foo",
                "/svc/my.service.Service".parse().unwrap(),
                mock_single_instance.clone(),
            )
            .add_outgoing_path(
                "bar",
                "/svc/my.service.Service".parse().unwrap(),
                mock_single_instance,
            )
            .build()
            .await;

        let root = test.model.root();
        test.create_dynamic_child(&Moniker::root(), "coll1", ChildBuilder::new().name("foo")).await;
        test.create_dynamic_child(&Moniker::root(), "coll1", ChildBuilder::new().name("bar")).await;
        let foo_component =
            root.find_and_maybe_resolve(&vec!["coll1:foo"].try_into().unwrap()).await.unwrap();

        let provider = MockAnonymizedCapabilityProvider {
            instances: Arc::new(Mutex::new(hashmap! {
                AggregateInstance::Child("coll1:foo".try_into().unwrap()) => foo_component.as_weak(),
                // "bar" not added to induce a routing failure on route_instance
            })),
        };

        let route = AnonymizedServiceRoute {
            source_moniker: Moniker::root(),
            members: vec![AggregateMember::Collection("coll1".parse().unwrap())],
            service_name: "my.service.Service".parse().unwrap(),
        };

        let dir =
            Arc::new(AnonymizedAggregateServiceDir::new(root.as_weak(), route, Box::new(provider)));

        dir.add_entries_from_children().await.unwrap();
        // Entries from foo should be available even though we can't route to bar
        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir.dir_entry().await);

        // List the entries of the directory served by `open`.
        let entries = fuchsia_fs::directory::readdir(&dir_proxy).await.unwrap();
        let instance_names: HashSet<String> = entries.into_iter().map(|d| d.name).collect();
        assert_eq!(instance_names.len(), 1);
        for instance in instance_names {
            assert!(is_instance_id(&instance), "{}", instance);
        }
    }

    #[fuchsia::test]
    async fn test_anonymized_service_directory_readdir() {
        let components = create_test_component_decls();

        let mock_instance_foo = pseudo_directory! {
            "default" => pseudo_directory! {}
        };

        let mock_instance_bar = pseudo_directory! {
            "default" => pseudo_directory! {},
            "one" => pseudo_directory! {},
        };

        let mock_instance_static_a = pseudo_directory! {
            "default" => pseudo_directory! {}
        };

        let test = RoutingTestBuilder::new("root", components)
            .add_outgoing_path("foo", "/svc/my.service.Service".parse().unwrap(), mock_instance_foo)
            .add_outgoing_path("bar", "/svc/my.service.Service".parse().unwrap(), mock_instance_bar)
            .add_outgoing_path(
                "static_a",
                "/svc/my.service.Service".parse().unwrap(),
                mock_instance_static_a,
            )
            .build()
            .await;

        let root = test.model.root();
        test.create_dynamic_child(&Moniker::root(), "coll1", ChildBuilder::new().name("foo")).await;
        test.create_dynamic_child(&Moniker::root(), "coll2", ChildBuilder::new().name("bar")).await;
        let foo_component =
            root.find_and_maybe_resolve(&vec!["coll1:foo"].try_into().unwrap()).await.unwrap();
        let bar_component =
            root.find_and_maybe_resolve(&vec!["coll2:bar"].try_into().unwrap()).await.unwrap();
        let static_a_component =
            root.find_and_maybe_resolve(&vec!["static_a"].try_into().unwrap()).await.unwrap();

        let provider = MockAnonymizedCapabilityProvider {
            instances: Arc::new(Mutex::new(hashmap! {
                AggregateInstance::Child("coll1:foo".try_into().unwrap()) => foo_component.as_weak(),
                AggregateInstance::Child("coll2:bar".try_into().unwrap()) => bar_component.as_weak(),
                AggregateInstance::Child("static_a".try_into().unwrap()) => static_a_component.as_weak(),
            })),
        };

        let route = AnonymizedServiceRoute {
            source_moniker: Moniker::root(),
            members: vec![
                AggregateMember::Collection("coll1".parse().unwrap()),
                AggregateMember::Collection("coll2".parse().unwrap()),
            ],
            service_name: "my.service.Service".parse().unwrap(),
        };

        let dir =
            Arc::new(AnonymizedAggregateServiceDir::new(root.as_weak(), route, Box::new(provider)));

        dir.add_entries_from_children().await.unwrap();

        let execution_scope = ExecutionScope::new();
        let dir_proxy = open_dir(execution_scope.clone(), dir.dir_entry().await);

        let entries = fuchsia_fs::directory::readdir(&dir_proxy).await.unwrap();

        let instance_names: HashSet<String> = entries.into_iter().map(|d| d.name).collect();
        assert_eq!(instance_names.len(), 4);
        for instance in instance_names {
            assert!(is_instance_id(&instance), "{}", instance);
        }
    }

    proptest! {
        #[test]
        fn service_instance_id(seed in 0..u64::MAX) {
            let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
            let instance = AnonymizedAggregateServiceDir::generate_instance_id(&mut rng);
            assert!(is_instance_id(&instance), "{}", instance);

            // Verify it's random
            let instance2 = AnonymizedAggregateServiceDir::generate_instance_id(&mut rng);
            assert!(is_instance_id(&instance2), "{}", instance2);
            assert_ne!(instance, instance2);
        }
    }

    fn is_instance_id(id: &str) -> bool {
        id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit())
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
                "/svc/my.service.Service".parse().unwrap(),
                mock_instance_foo.clone(),
            )
            .build()
            .await;

        let root = test.model.root();
        test.create_dynamic_child(&Moniker::root(), "coll1", ChildBuilder::new().name("foo")).await;
        let foo_component = root
            .find_and_maybe_resolve(&vec!["coll1:foo"].try_into().unwrap())
            .await
            .expect("failed to find foo instance");
        let provider = MockOfferCapabilityProvider {
            component: foo_component.as_weak(),
            instance_filter: vec![
                NameMapping {
                    source_name: "default".parse().unwrap(),
                    target_name: "a".parse().unwrap(),
                },
                NameMapping {
                    source_name: "default".parse().unwrap(),
                    target_name: "b".parse().unwrap(),
                },
                NameMapping {
                    source_name: "one".parse().unwrap(),
                    target_name: "two".parse().unwrap(),
                },
            ],
        };

        let dir = FilteredAggregateServiceDir::new(
            root.as_weak(),
            foo_component.as_weak(),
            Box::new(provider),
        )
        .await
        .unwrap();
        let dir_proxy = open_dir(ExecutionScope::new(), dir);
        let entries = fuchsia_fs::directory::readdir(&dir_proxy).await.unwrap();
        let entries: Vec<_> = entries.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(entries, vec!["a", "b", "two"]);
    }
}
