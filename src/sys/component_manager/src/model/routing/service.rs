// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilityProvider,
        model::{
            component::{ComponentInstance, WeakComponentInstance, WeakExtendedInstance},
            error::{CapabilityProviderError, ModelError, OpenError},
            hooks::{Event, EventPayload, EventType, Hook, HooksRegistration},
            mutable_directory::MutableDirectory,
            routing::{CapabilitySource, OpenOptions, OpenRequest, RouteSource, RoutingError},
        },
    },
    async_trait::async_trait,
    cm_rust::{CapabilityTypeName, ComponentDecl, ExposeDecl, ExposeDeclCommon},
    cm_types::Name,
    cm_util::channel,
    cm_util::TaskGroup,
    fidl::{endpoints::ServerEnd, epitaph::ChannelEpitaphExt},
    fidl_fuchsia_io as fio,
    flyweights::FlyStr,
    fuchsia_async::{DurationExt, TimeoutExt},
    fuchsia_zircon as zx,
    futures::future::{join_all, BoxFuture},
    futures::lock::Mutex,
    futures::FutureExt,
    moniker::{ChildName, ExtendedMoniker, Moniker, MonikerBase},
    routing::capability_source::{
        CollectionAggregateCapabilityProvider, OfferAggregateCapabilityProvider,
    },
    std::{
        collections::HashMap,
        fmt,
        path::PathBuf,
        sync::{Arc, Weak},
    },
    tracing::{error, warn},
    vfs::{
        directory::{
            entry::{DirectoryEntry, EntryInfo},
            immutable::simple::{simple as simple_immutable_dir, Simple as SimpleImmutableDir},
        },
        execution_scope::ExecutionScope,
    },
};

/// Timeout for opening a service capability when aggregating.
const OPEN_SERVICE_TIMEOUT: zx::Duration = zx::Duration::from_seconds(5);

/// Serves a Service directory that allows clients to list instances resulting from an aggregation of service offers
/// and to open instances.
///
pub struct AggregateServiceDirectoryProvider {
    /// Execution scope for requests to `dir`. This is the same scope
    /// as the one in `collection_component`'s resolved state.
    execution_scope: ExecutionScope,

    /// The directory that contains entries for all service instances
    /// across all of the aggregated source services.
    dir: Arc<SimpleImmutableDir>,
}

impl AggregateServiceDirectoryProvider {
    pub async fn new(
        parent: WeakComponentInstance,
        target: WeakComponentInstance,
        provider: Box<dyn OfferAggregateCapabilityProvider<ComponentInstance>>,
    ) -> Result<AggregateServiceDirectoryProvider, ModelError> {
        let execution_scope =
            parent.upgrade()?.lock_resolved_state().await?.execution_scope().clone();
        let dir = AggregateServiceDirectory::new(parent, target, provider).await?;
        Ok(AggregateServiceDirectoryProvider { execution_scope, dir })
    }
}

#[async_trait]
impl CapabilityProvider for AggregateServiceDirectoryProvider {
    async fn open(
        self: Box<Self>,
        _task_group: TaskGroup,
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), CapabilityProviderError> {
        let relative_path_utf8 = relative_path.to_str().ok_or(CapabilityProviderError::BadPath)?;
        let relative_path = if relative_path_utf8.is_empty() {
            vfs::path::Path::dot()
        } else {
            vfs::path::Path::validate_and_split(relative_path_utf8)
                .map_err(|_| CapabilityProviderError::BadPath)?
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
struct AggregateServiceDirectory {}

impl AggregateServiceDirectory {
    pub async fn new(
        parent: WeakComponentInstance,
        target: WeakComponentInstance,
        provider: Box<dyn OfferAggregateCapabilityProvider<ComponentInstance>>,
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
                                .with_logger_as_default(|| {
                                    warn!(
                                        parent=%parent.moniker, %e,
                                        "Failed to route aggregate service instance",
                                    );
                                })
                                .await;
                        }
                        return vec![];
                    }
                };
                let capability_source = Arc::new(route_data.capability_source);
                let entries: Vec<_> = route_data
                    .instance_filter
                    .into_iter()
                    .map(|mapping| {
                        Arc::new(ServiceInstanceDirectoryEntry::<FlyStr> {
                            name: mapping.target_name,
                            capability_source: capability_source.clone(),
                            source_id: mapping.source_name.clone().into(),
                            service_instance: mapping.source_name.clone().into(),
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

/// Represents a routed service capability from a collection in the source component.
#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub struct CollectionServiceRoute {
    /// Moniker of the component that contains the collection.
    pub source_moniker: Moniker,

    /// All collections over which the aggregated service is exposed.
    pub collections: Vec<Name>,

    /// Name of the service exposed from the collection.
    pub service_name: Name,
}

impl CollectionServiceRoute {
    /// Returns true if the component with `moniker` is a child of the collection in this route.
    fn matches_child_component(&self, moniker: &Moniker) -> bool {
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
                .map_or(false, |collection| self.collections.contains(collection.into()))
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
    entries: HashMap<
        ServiceInstanceDirectoryKey<ChildName>,
        Arc<ServiceInstanceDirectoryEntry<ChildName>>,
    >,
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
    aggregate_capability_provider:
        Box<dyn CollectionAggregateCapabilityProvider<ComponentInstance>>,

    inner: Mutex<CollectionServiceDirectoryInner>,
}

impl CollectionServiceDirectory {
    pub fn new(
        parent: WeakComponentInstance,
        route: CollectionServiceRoute,
        aggregate_capability_provider: Box<
            dyn CollectionAggregateCapabilityProvider<ComponentInstance>,
        >,
    ) -> Self {
        CollectionServiceDirectory {
            parent,
            route,
            aggregate_capability_provider,
            inner: Mutex::new(CollectionServiceDirectoryInner {
                dir: simple_immutable_dir(),
                entries: HashMap::new(),
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

    /// Returns metadata about all the service instances in their original representation,
    /// useful for exposing debug info. The results are returned in no particular order.
    pub async fn entries(&self) -> Vec<Arc<ServiceInstanceDirectoryEntry<ChildName>>> {
        self.inner.lock().await.entries.values().cloned().collect()
    }

    /// Adds directory entries from services exposed by a child in the aggregated collection.
    pub async fn add_entries_from_child(&self, moniker: &ChildName) -> Result<(), ModelError> {
        let parent =
            self.parent.upgrade().map_err(|err| ModelError::ComponentInstanceError { err })?;
        match self.aggregate_capability_provider.route_instance(moniker).await {
            Ok(source) => {
                // Add entries for the component `moniker`, from its `source`,
                // the service exposed by the component.
                if let Err(err) = self.add_entries_from_capability_source(moniker, source).await {
                    parent
                        .with_logger_as_default(|| {
                            error!(
                                component=%moniker,
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
                            component=%moniker,
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
        moniker: &ChildName,
        source: CapabilitySource,
    ) -> Result<(), ModelError> {
        let task_group = self.parent.upgrade()?.nonblocking_task_group();
        // Lazily add entries for instances from the service exposed by this component.
        // This has to happen after this function, the Started hook handler, returns because
        // `add_entries_from_capability_source` reads from the exposed service directory
        // to enumerates service instances, but this directory is not served until the
        // component is actually running. The component is only started *after* all hooks run.
        let moniker = moniker.clone();
        let add_instances_to_dir = async move {
            self.add_entries_from_capability_source(&moniker, source)
                .then(|result| async {
                    if let Err(e) = result {
                        error!(error = ?e, "failed to add service instances");
                    }
                })
                .await;
        };
        task_group.spawn(add_instances_to_dir).await;
        Ok(())
    }

    /// Opens the service capability at `source` and adds entries for each service instance.
    ///
    /// Entry names are named by joining the originating child component instance,
    /// `child_name`, and the service instance name, with a comma.
    ///
    /// # Errors
    /// Returns an error if `source` is not a service capability, or could not be opened.
    pub async fn add_entries_from_capability_source(
        &self,
        moniker: &ChildName,
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

        OpenRequest::new_from_route_source(
            RouteSource { source: source.clone(), relative_path: "".into() },
            &target,
            OpenOptions {
                flags: fio::OpenFlags::DIRECTORY,
                relative_path: "".into(),
                server_chan: &mut server.into_channel(),
            },
        )
        .open()
        .on_timeout(OPEN_SERVICE_TIMEOUT.after_now(), || Err(OpenError::Timeout))
        .await?;
        let dirents = fuchsia_fs::directory::readdir(&proxy).await.map_err(|e| {
            error!(
                "Error reading entries from service directory for component '{}', \
                capability name '{}'. Error: {}",
                target.moniker.clone(),
                source.source_name().map(Name::as_str).unwrap_or("<no capability>"),
                e
            );
            ModelError::open_directory_error(target.moniker.clone(), moniker.to_string())
        })?;
        let rng = &mut rand::thread_rng();
        for dirent in dirents {
            let instance_key = ServiceInstanceDirectoryKey::<ChildName> {
                source_id: moniker.clone(),
                service_instance: FlyStr::new(&dirent.name),
            };
            // It's possible to enter this function multiple times with the same input, so
            // check for duplicates.
            if inner.entries.contains_key(&instance_key) {
                continue;
            }
            let name = Self::generate_instance_id(rng);
            let entry: Arc<ServiceInstanceDirectoryEntry<ChildName>> =
                Arc::new(ServiceInstanceDirectoryEntry::<ChildName> {
                    name: name.clone(),
                    capability_source: Arc::new(source.clone()),
                    source_id: instance_key.source_id.clone(),
                    service_instance: instance_key.service_instance.clone(),
                });
            inner.dir.add_node(&name, entry.clone()).map_err(|err| {
                ModelError::ServiceDirError { moniker: target.moniker.clone(), err }
            })?;
            inner.entries.insert(instance_key, entry);
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
                |instance| async move {
                    self.add_entries_from_child(&instance).await.map_err(|e| {
                        error!(error=%e, child=%instance, "error adding entries from child");
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
            let capability_source =
                self.aggregate_capability_provider.route_instance(child_moniker).await?;

            self.add_entries_from_capability_source_lazy(child_moniker, capability_source).await?;
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
                if entry.source_id == *target_child_moniker {
                    inner.dir.remove_node(&entry.name).map_err(|err| {
                        ModelError::ServiceDirError { moniker: target_moniker.clone(), err }
                    })?;
                }
            }
            inner.entries.retain(|key, _| key.source_id != *target_child_moniker);
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
pub struct ServiceInstanceDirectoryEntry<T: Send + Sync + 'static + fmt::Display> {
    /// The name of the entry in its parent directory.
    pub name: String,

    /// The source of the service capability instance to route.
    capability_source: Arc<CapabilitySource>,

    /// An identifier that can be used to find the child component that serves the service
    /// instance.
    /// This is a generic type because it varies between aggregated directory types. For example,
    /// for aggregated offers this an instance in the source instance filter,
    /// while for aggregated collections it is the moniker of the source child.
    // TODO(fxbug.dev/4776): CollectionServiceDirectory needs this, but AggregateServiceDirectory
    // only uses this for debug info. We could probably have CollectionServiceDirectory use
    // ServiceInstanceDirectoryKey.source_id instead, and either delete this or make it debug-only.
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

impl<T: Send + Sync + 'static + fmt::Display> DirectoryEntry for ServiceInstanceDirectoryEntry<T> {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        mut path: vfs::path::Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let mut server_end = server_end.into_channel();
        let source_component = match self.capability_source.source_instance() {
            WeakExtendedInstance::Component(c) => c,
            WeakExtendedInstance::AboveRoot(_) => {
                unreachable!(
                    "aggregate service directory has a capability source above root, but this is \
                impossible"
                );
            }
        };
        let Ok(source_component) = source_component.upgrade() else {
            warn!(moniker=%source_component.moniker, "source_component of aggregated service directory is gone");
            return;
        };
        // VFS paths are canonicalized so this will do the right thing.
        let mut relative_path = PathBuf::new();
        relative_path.push(self.service_instance.as_str());
        while let Some(p) = path.next() {
            relative_path.push(p);
        }
        let relative_path = relative_path.to_string_lossy().to_string();
        let route_source = RouteSource::new((*self.capability_source).clone());
        scope.spawn(async move {
            let open_options = OpenOptions { flags, relative_path, server_chan: &mut server_end };
            let open_request =
                OpenRequest::new_from_route_source(route_source, &source_component, open_options);
            if let Err(err) = open_request.open().await {
                server_end
                    .close_with_epitaph(err.as_zx_status())
                    .unwrap_or_else(|error| warn!(%error, "failed to close server end"));
                source_component
                    .with_logger_as_default(|| {
                        error!(
                            service_instance=%self.service_instance,
                            source_instance=%source_component.moniker,
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
                testing::routing_test_helpers::{RoutingTest, RoutingTestBuilder},
            },
        },
        ::routing::{
            capability_source::{
                CollectionAggregateCapabilityProvider, ComponentCapability,
                OfferAggregateCapabilityProvider, OfferAggregateCapabilityRouteData,
            },
            component_instance::ComponentInstanceInterface,
            error::RoutingError,
        },
        cm_rust::*,
        cm_rust_testing::{ChildDeclBuilder, CollectionDeclBuilder, ComponentDeclBuilder},
        fuchsia_async as fasync,
        moniker::{ChildName, Moniker, MonikerBase},
        proptest::prelude::*,
        rand::SeedableRng,
        std::{
            collections::{HashMap, HashSet},
            convert::TryInto,
        },
        vfs::pseudo_directory,
    };

    #[derive(Clone)]
    struct MockCollectionCapabilityProvider {
        instances: HashMap<ChildName, WeakComponentInstance>,
    }

    #[async_trait]
    impl CollectionAggregateCapabilityProvider<ComponentInstance> for MockCollectionCapabilityProvider {
        async fn route_instance(
            &self,
            instance: &ChildName,
        ) -> Result<CapabilitySource, RoutingError> {
            Ok(CapabilitySource::Component {
                capability: ComponentCapability::Service(ServiceDecl {
                    name: "my.service.Service".parse().unwrap(),
                    source_path: Some("/svc/my.service.Service".parse().unwrap()),
                }),
                component: self
                    .instances
                    .get(instance)
                    .ok_or_else(|| RoutingError::OfferFromChildInstanceNotFound {
                        capability_id: "my.service.Service".to_string(),
                        child_moniker: instance.clone(),
                        moniker: Moniker::root(),
                    })?
                    .clone(),
            })
        }

        async fn list_instances(&self) -> Result<Vec<ChildName>, RoutingError> {
            Ok(self.instances.keys().cloned().collect())
        }

        fn clone_boxed(&self) -> Box<dyn CollectionAggregateCapabilityProvider<ComponentInstance>> {
            Box::new(self.clone())
        }
    }

    #[derive(Clone)]
    struct MockOfferCapabilityProvider {
        component: WeakComponentInstance,
        instance_filter: Vec<NameMapping>,
    }

    #[async_trait]
    impl OfferAggregateCapabilityProvider<ComponentInstance> for MockOfferCapabilityProvider {
        fn route_instances(
            &self,
        ) -> Vec<
            BoxFuture<
                '_,
                Result<OfferAggregateCapabilityRouteData<ComponentInstance>, RoutingError>,
            >,
        > {
            let capability_source = CapabilitySource::Component {
                capability: ComponentCapability::Service(ServiceDecl {
                    name: "my.service.Service".parse().unwrap(),
                    source_path: Some("/svc/my.service.Service".parse().unwrap()),
                }),
                component: self.component.clone(),
            };
            let data = OfferAggregateCapabilityRouteData::<ComponentInstance> {
                capability_source,
                instance_filter: self.instance_filter.clone(),
            };
            let fut = async move { Ok(data) };
            vec![Box::pin(fut)]
        }

        fn clone_boxed(&self) -> Box<dyn OfferAggregateCapabilityProvider<ComponentInstance>> {
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
            fio::OpenFlags::DIRECTORY,
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
                        source_name: "fuchsia.component.Realm".parse().unwrap(),
                        target_path: "/svc/fuchsia.component.Realm".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Collection("coll1".parse().unwrap()),
                        source_name: "my.service.Service".parse().unwrap(),
                        target_name: "my.service.Service".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Collection("coll2".parse().unwrap()),
                        source_name: "my.service.Service".parse().unwrap(),
                        target_name: "my.service.Service".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .add_collection(CollectionDeclBuilder::new_transient_collection("coll1"))
                    .add_collection(CollectionDeclBuilder::new_transient_collection("coll2"))
                    .build(),
            ),
            (
                "foo",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "my.service.Service".parse().unwrap(),
                        target_name: "my.service.Service".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(ServiceDecl {
                        name: "my.service.Service".parse().unwrap(),
                        source_path: Some("/svc/my.service.Service".parse().unwrap()),
                    })
                    .build(),
            ),
            (
                "bar",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "my.service.Service".parse().unwrap(),
                        target_name: "my.service.Service".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(ServiceDecl {
                        name: "my.service.Service".parse().unwrap(),
                        source_path: Some("/svc/my.service.Service".parse().unwrap()),
                    })
                    .build(),
            ),
            (
                "baz",
                ComponentDeclBuilder::new()
                    .expose(ExposeDecl::Service(ExposeServiceDecl {
                        source: ExposeSource::Self_,
                        source_name: "my.service.Service".parse().unwrap(),
                        target_name: "my.service.Service".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .service(ServiceDecl {
                        name: "my.service.Service".parse().unwrap(),
                        source_path: Some("/svc/my.service.Service".parse().unwrap()),
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
                "/svc/my.service.Service".parse().unwrap(),
                mock_single_instance.clone(),
            )
            .add_outgoing_path(
                "bar",
                "/svc/my.service.Service".parse().unwrap(),
                mock_single_instance,
            )
            .add_outgoing_path(
                "baz",
                "/svc/my.service.Service".parse().unwrap(),
                mock_dual_instance,
            )
            .build()
            .await;

        test.create_dynamic_child(
            &Moniker::root(),
            "coll1",
            ChildDeclBuilder::new_lazy_child("foo"),
        )
        .await;
        test.create_dynamic_child(
            &Moniker::root(),
            "coll1",
            ChildDeclBuilder::new_lazy_child("bar"),
        )
        .await;
        test.create_dynamic_child(
            &Moniker::root(),
            "coll2",
            ChildDeclBuilder::new_lazy_child("baz"),
        )
        .await;
        let foo_component = test
            .model
            .look_up(&vec!["coll1:foo"].try_into().unwrap())
            .await
            .expect("failed to find foo instance");
        let bar_component = test
            .model
            .look_up(&vec!["coll1:bar"].try_into().unwrap())
            .await
            .expect("failed to find bar instance");
        let baz_component = test
            .model
            .look_up(&vec!["coll2:baz"].try_into().unwrap())
            .await
            .expect("failed to find baz instance");

        let provider = MockCollectionCapabilityProvider {
            instances: {
                let mut instances = HashMap::new();
                instances.insert("coll1:foo".try_into().unwrap(), foo_component.as_weak());
                instances.insert("coll1:bar".try_into().unwrap(), bar_component.as_weak());
                instances.insert("coll2:baz".try_into().unwrap(), baz_component.as_weak());
                instances
            },
        };

        let route = CollectionServiceRoute {
            source_moniker: Moniker::root(),
            collections: vec!["coll1".parse().unwrap(), "coll2".parse().unwrap()],
            service_name: "my.service.Service".parse().unwrap(),
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

        // List the entries of the directory served by `open`, and compare them to the
        // internal state.
        let instance_names = {
            let instance_names: HashSet<_> =
                dir_arc.entries().await.into_iter().map(|e| e.name.clone()).collect();
            let dir_contents = fuchsia_fs::directory::readdir(&dir_proxy)
                .await
                .expect("failed to read directory entries");
            let dir_instance_names: HashSet<_> = dir_contents.into_iter().map(|d| d.name).collect();
            assert_eq!(instance_names.len(), 4);
            assert_eq!(dir_instance_names, instance_names);
            instance_names
        };

        // Open one of the entries.
        {
            let instance_dir = fuchsia_fs::directory::open_directory(
                &dir_proxy,
                instance_names.iter().next().expect("failed to get instance name"),
                fio::OpenFlags::empty(),
            )
            .await
            .expect("failed to open collection dir");

            // Make sure we're reading the expected directory.
            let instance_dir_contents = fuchsia_fs::directory::readdir(&instance_dir)
                .await
                .expect("failed to read instances of collection dir");
            assert!(instance_dir_contents.iter().find(|d| d.name == "member").is_some());
        }

        let baz_component = test
            .model
            .look_up(&vec!["coll2:baz"].try_into().unwrap())
            .await
            .expect("failed to find baz instance");

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

        // Test that removal of instances works.
        {
            baz_component.stop().await.expect("failed to shutdown component");
            let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
            assert_eq!(dir_contents.len(), 2);

            test.start_instance_and_wait_start(baz_component.moniker())
                .await
                .expect("component should start");

            let dir_contents = wait_for_dir_content_change(&dir_proxy, dir_contents).await;
            assert_eq!(dir_contents.len(), 4);
        }
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

        let foo_component = test
            .model
            .look_up(&vec!["coll1:foo"].try_into().unwrap())
            .await
            .expect("failed to find foo instance");

        // Test that starting an instance results in the collection service directory adding the
        // relevant instances.
        foo_component.start(&StartReason::Eager, None, vec![], vec![]).await.unwrap();
        let entries = wait_for_dir_content_change(&dir_proxy, entries).await;
        assert_eq!(entries.len(), 1);

        let baz_component = test
            .model
            .look_up(&vec!["coll2:baz"].try_into().unwrap())
            .await
            .expect("failed to find baz instance");

        // Test with second collection
        baz_component.start(&StartReason::Eager, None, vec![], vec![]).await.unwrap();
        let entries = wait_for_dir_content_change(&dir_proxy, entries).await;
        assert_eq!(entries.len(), 3);
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

        let foo_component = test
            .model
            .look_up(&vec!["coll1:foo"].try_into().unwrap())
            .await
            .expect("failed to find baz instance");

        // Test that removal of instances works
        foo_component.stop().await.expect("failed to shutdown component");
        let entries = wait_for_dir_content_change(&dir_proxy, entries).await;
        assert_eq!(entries.len(), 3);

        let baz_component = test
            .model
            .look_up(&vec!["coll2:baz"].try_into().unwrap())
            .await
            .expect("failed to find baz instance");

        // Test with second collection
        baz_component.stop().await.expect("failed to shutdown component");
        let entries = wait_for_dir_content_change(&dir_proxy, entries).await;
        assert_eq!(entries.len(), 1);
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

        test.create_dynamic_child(
            &Moniker::root(),
            "coll1",
            ChildDeclBuilder::new_lazy_child("foo"),
        )
        .await;
        test.create_dynamic_child(
            &Moniker::root(),
            "coll1",
            ChildDeclBuilder::new_lazy_child("bar"),
        )
        .await;
        let foo_component = test
            .model
            .look_up(&vec!["coll1:foo"].try_into().unwrap())
            .await
            .expect("failed to find foo instance");

        let provider = MockCollectionCapabilityProvider {
            instances: {
                let mut instances = HashMap::new();
                instances.insert("coll1:foo".try_into().unwrap(), foo_component.as_weak());
                // "bar" not added to induce a routing failure on route_instance
                instances
            },
        };

        let route = CollectionServiceRoute {
            source_moniker: Moniker::root(),
            collections: vec!["coll1".parse().unwrap()],
            service_name: "my.service.Service".parse().unwrap(),
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
        for instance in instance_names {
            assert!(is_instance_id(&instance), "{}", instance);
        }
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
            .add_outgoing_path("foo", "/svc/my.service.Service".parse().unwrap(), mock_instance_foo)
            .add_outgoing_path("bar", "/svc/my.service.Service".parse().unwrap(), mock_instance_bar)
            .build()
            .await;

        test.create_dynamic_child(
            &Moniker::root(),
            "coll1",
            ChildDeclBuilder::new_lazy_child("foo"),
        )
        .await;
        test.create_dynamic_child(
            &Moniker::root(),
            "coll2",
            ChildDeclBuilder::new_lazy_child("bar"),
        )
        .await;
        let foo_component = test
            .model
            .look_up(&vec!["coll1:foo"].try_into().unwrap())
            .await
            .expect("failed to find foo instance");
        let bar_component = test
            .model
            .look_up(&vec!["coll2:bar"].try_into().unwrap())
            .await
            .expect("failed to find bar instance");

        let provider = MockCollectionCapabilityProvider {
            instances: {
                let mut instances = HashMap::new();
                instances.insert("coll1:foo".try_into().unwrap(), foo_component.as_weak());
                instances.insert("coll2:bar".try_into().unwrap(), bar_component.as_weak());
                instances
            },
        };

        let route = CollectionServiceRoute {
            source_moniker: Moniker::root(),
            collections: vec!["coll1".parse().unwrap(), "coll2".parse().unwrap()],
            service_name: "my.service.Service".parse().unwrap(),
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
        assert_eq!(instance_names.len(), 3);
        for instance in instance_names {
            assert!(is_instance_id(&instance), "{}", instance);
        }
    }

    proptest! {
        #[test]
        fn service_instance_id(seed in 0..u64::MAX) {
            let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
            let instance = CollectionServiceDirectory::generate_instance_id(&mut rng);
            assert!(is_instance_id(&instance), "{}", instance);

            // Verify it's random
            let instance2 = CollectionServiceDirectory::generate_instance_id(&mut rng);
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

        test.create_dynamic_child(
            &Moniker::root(),
            "coll1",
            ChildDeclBuilder::new_lazy_child("foo"),
        )
        .await;
        let foo_component = test
            .model
            .look_up(&vec!["coll1:foo"].try_into().unwrap())
            .await
            .expect("failed to find foo instance");
        let provider = MockOfferCapabilityProvider {
            component: foo_component.as_weak(),
            instance_filter: vec![
                NameMapping { source_name: "default".into(), target_name: "a".into() },
                NameMapping { source_name: "default".into(), target_name: "b".into() },
                NameMapping { source_name: "one".into(), target_name: "two".into() },
            ],
        };

        let dir = AggregateServiceDirectory::new(
            test.model.root().as_weak(),
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
