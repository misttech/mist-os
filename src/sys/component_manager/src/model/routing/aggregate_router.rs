// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::{ComponentInstance, WeakComponentInstance};
use crate::model::routing::service::{
    AnonymizedAggregateCapabilityProvider, AnonymizedAggregateServiceDir, AnonymizedServiceRoute,
};
use async_trait::async_trait;
use cm_types::Name;
use fidl::endpoints::create_proxy;
use fidl_fuchsia_io as fio;
use fuchsia_fs::directory::readdir;
use futures::lock::Mutex;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use router_error::RouterError;
use routing::bedrock::aggregate_router::AggregateSource;
use routing::capability_source::{
    AggregateInstance, AnonymizedAggregateSource, CapabilitySource,
    FilteredAggregateProviderSource, ServiceInstance,
};
use routing::component_instance::ComponentInstanceInterface;
use routing::error::{ComponentInstanceError, RoutingError};
use sandbox::{
    Capability, Data, Dict, DirEntry, RemotableCapability, Request, Router, RouterResponse,
};
use std::cmp::Ordering;
use std::sync::Arc;
use vfs::directory::entry::SubNode;
use vfs::execution_scope::ExecutionScope;

#[derive(Debug, Clone)]
enum AnonymizedOrFiltered {
    AnonymizedAggregate(AnonymizedAggregateSource),
    FilteredAggregateProvider(FilteredAggregateProviderSource),
}

impl From<AnonymizedOrFiltered> for CapabilitySource {
    fn from(source: AnonymizedOrFiltered) -> CapabilitySource {
        match source {
            AnonymizedOrFiltered::AnonymizedAggregate(source) => {
                CapabilitySource::AnonymizedAggregate(source)
            }
            AnonymizedOrFiltered::FilteredAggregateProvider(source) => {
                CapabilitySource::FilteredAggregateProvider(source)
            }
        }
    }
}

/// A router which will return a directory that aggregates together entries from the `sources`.
pub struct AggregateRouter {
    component: WeakComponentInstance,
    capability_source: AnonymizedOrFiltered,
    sources: Vec<AggregateSource>,
    // This is set to `Some` when the aggregate router has been started.
    aggregate_directory: Mutex<Option<DirEntry>>,
    // This is set to `Some` when the aggregate router has been started, if this is an anonymizing
    // aggregate.
    anonymized_aggregate_service_dir: Mutex<Option<Arc<AnonymizedAggregateServiceDir>>>,
    scope: ExecutionScope,
}

#[async_trait]
impl sandbox::Routable<DirEntry> for AggregateRouter {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<DirEntry>, RouterError> {
        let aggregate_dir = self.get_aggregate_dir(request).await?;
        if debug {
            let data: Data = self
                .get_capability_source_with_instances()
                .await
                .try_into()
                .expect("failed to persist capability source");
            return Ok(RouterResponse::Debug(data));
        }

        return Ok(RouterResponse::Capability(aggregate_dir));
    }
}

impl AggregateRouter {
    /// Creates a new aggregate router. It will either filter/rename entries or it will aggregate
    /// them together, depending on the value of `capability_source`.
    #[allow(unused)]
    pub fn new(
        component: Arc<ComponentInstance>,
        sources: Vec<AggregateSource>,
        capability_source: CapabilitySource,
    ) -> Router<DirEntry> {
        let capability_source = match capability_source {
            CapabilitySource::AnonymizedAggregate(source) => {
                AnonymizedOrFiltered::AnonymizedAggregate(source)
            }
            CapabilitySource::FilteredAggregateProvider(source) => {
                AnonymizedOrFiltered::FilteredAggregateProvider(source)
            }
            source => panic!("non-aggregate source passed to aggregate router: {:?}", source),
        };
        Router::new(Self {
            component: component.as_weak(),
            capability_source,
            sources,
            aggregate_directory: Mutex::new(None),
            anonymized_aggregate_service_dir: Mutex::new(None),
            scope: component.execution_scope.clone(),
        })
    }

    /// Returns `self.capability_source` with the `instances` field filled in if this is an
    /// anonymizing aggregate.
    async fn get_capability_source_with_instances(&self) -> CapabilitySource {
        let mut anonymized_aggregate_source = match &self.capability_source {
            AnonymizedOrFiltered::AnonymizedAggregate(source) => source.clone(),
            AnonymizedOrFiltered::FilteredAggregateProvider(source) => {
                return CapabilitySource::FilteredAggregateProvider(source.clone())
            }
        };

        let mut instances = vec![];
        let service_dir_guard = self.anonymized_aggregate_service_dir.lock().await;
        if let Some(anonymized_aggregate_service_dir) = &*service_dir_guard {
            let mut dir_entries = anonymized_aggregate_service_dir.entries().await;
            // Sort the entries (they can show up in any order)
            dir_entries.sort_by(|a, b| match a.source_id.cmp(&b.source_id) {
                Ordering::Equal => a.service_instance.cmp(&b.service_instance),
                o => o,
            });
            for service_entry in dir_entries {
                instances.push(ServiceInstance {
                    instance_name: service_entry.name.clone(),
                    child_name: format!("{}", &service_entry.source_id),
                    child_instance_name: Name::new(service_entry.service_instance.clone()).unwrap(),
                });
            }
        }
        anonymized_aggregate_source.instances = instances;
        CapabilitySource::AnonymizedAggregate(anonymized_aggregate_source)
    }

    /// Returns the directory containing aggregated entries, and initializes it if necessary.
    async fn get_aggregate_dir(&self, request: Option<Request>) -> Result<DirEntry, RouterError> {
        let mut maybe_directory = self.aggregate_directory.lock().await;
        if let Some(aggregate_directory) = &*maybe_directory {
            return Ok(aggregate_directory.clone());
        }
        // We hold the `self.aggregate_directory` lock going forward because we want to ensure that
        // only one task is doing this work. Multiple tasks can't make this go any faster, and once
        // we're done the following tasks can immediately grab a clone of the directory we create.

        let request = request.ok_or_else(|| RouterError::InvalidArgs)?;

        let aggregate_directory = match &self.capability_source {
            AnonymizedOrFiltered::AnonymizedAggregate(anonymized_source) => {
                self.create_anonymized_aggregate(anonymized_source).await?
            }
            AnonymizedOrFiltered::FilteredAggregateProvider(_) => {
                self.create_filtered_aggregate(request).await?
            }
        };

        *maybe_directory = Some(aggregate_directory.clone());
        Ok(aggregate_directory)
    }

    async fn create_anonymized_aggregate(
        &self,
        anonymized_source: &AnonymizedAggregateSource,
    ) -> Result<DirEntry, RouterError> {
        let route = AnonymizedServiceRoute {
            source_moniker: self.component.moniker.clone(),
            members: anonymized_source.members.clone(),
            service_name: anonymized_source.capability.source_name().clone(),
        };
        let aggregate_capability_provider = AnonymizedAggregateServiceProvider {
            component: self.component.clone(),
            sources: self.sources.clone(),
            service_name: anonymized_source.capability.source_name().clone(),
        };
        let service_dir = Arc::new(AnonymizedAggregateServiceDir::new(
            self.component.clone(),
            route,
            Box::new(aggregate_capability_provider),
        ));
        // Errors returned by this function are already logged, so there's not much for us to do if
        // it returns an error. Some children may have been added successfully, so we might as well
        // continue.
        self.component
            .upgrade()
            .map_err(RoutingError::from)?
            .hooks
            .install(service_dir.hooks())
            .await;
        let _ = service_dir.add_entries_from_children().await;
        *self.anonymized_aggregate_service_dir.lock().await = Some(service_dir.clone());
        Ok(DirEntry::new(service_dir.dir_entry().await))
    }

    async fn create_filtered_aggregate(&self, request: Request) -> Result<DirEntry, RouterError> {
        let source_dir_routers = self.sources.iter().filter_map(|source| match source {
            AggregateSource::DirectoryRouter { source_instance: _, router } => Some(router),
            AggregateSource::Collection { collection_name: _ } => panic!("collections can't contribute to filtered aggregates, manifest validation should stop this"),
        });
        let mut routing_futures = FuturesUnordered::new();
        for router in source_dir_routers {
            routing_futures.push(router.route(Some(request.try_clone()?), false));
        }
        let aggregate_dictionary = Dict::new();
        while let Some(router_response) = routing_futures.next().await {
            let source_dir = match router_response {
                Ok(RouterResponse::Capability(dir_entry)) => dir_entry,
                Ok(RouterResponse::Unavailable) => {
                    // If the capability is unavailable, then there's nothing for us to do here.
                    continue;
                }
                Ok(RouterResponse::Debug(_)) => {
                    panic!("unexpected debug result from non-debug route");
                }
                Err(router_error) => {
                    log::warn!(
                        "failed to route service capability for aggregate: {:?}",
                        router_error
                    );
                    continue;
                }
            };
            let (source_dir_proxy, server_end) = create_proxy::<fio::DirectoryMarker>();
            source_dir.open(
                self.scope.clone(),
                fio::OpenFlags::RIGHT_READABLE,
                ".",
                server_end.into_channel(),
            );
            // Renames have already been applied by the `with_service_renames_and_filter` function
            // in `source_dir`, so we don't need to do any remappings here.
            let parent_dir = source_dir.try_into_directory_entry(self.scope.clone()).unwrap();
            for entry in readdir(&source_dir_proxy).await.unwrap_or_default() {
                if &entry.name == "." || entry.kind != fuchsia_fs::directory::DirentKind::Directory
                {
                    continue;
                }
                let path = vfs::path::Path::validate_and_split(&entry.name)
                    .expect("path returned from VFS is invalid");
                let sub_dir_entry = DirEntry::new(Arc::new(SubNode::new(
                    parent_dir.clone(),
                    path,
                    fio::DirentType::Directory,
                )));
                let name = entry.name.parse().expect("path returned from VFS is not a valid name");
                aggregate_dictionary.insert(name, sub_dir_entry.into()).expect(
                    "failed to insert into aggregate dictionary, name collisions should be \
                            prevented by manifest validation",
                );
            }
        }
        Ok(DirEntry::new(
            aggregate_dictionary.try_into_directory_entry(self.scope.clone()).unwrap(),
        )
        .into())
    }
}

struct AnonymizedAggregateServiceProvider {
    component: WeakComponentInstance,
    sources: Vec<AggregateSource>,
    service_name: Name,
}

#[async_trait]
impl AnonymizedAggregateCapabilityProvider for AnonymizedAggregateServiceProvider {
    async fn list_instances(&self) -> Result<Vec<AggregateInstance>, RoutingError> {
        let mut instances = vec![];
        for source in &self.sources {
            match source {
                AggregateSource::DirectoryRouter { source_instance, router: _ } => {
                    instances.push(source_instance.clone())
                }
                AggregateSource::Collection { collection_name } => {
                    let children_in_collection = {
                        let component = self.component.upgrade()?;
                        let resolved_state =
                            component.lock_resolved_state().await.map_err(|e| {
                                ComponentInstanceError::ResolveFailed {
                                    moniker: self.component.moniker.clone(),
                                    err: anyhow::format_err!("{:?}", e).into(),
                                }
                            })?;
                        resolved_state.children_in_collection(collection_name)
                    };
                    for (child_name, child_instance) in children_in_collection {
                        // If we can't resolve the child, then it's not contributing to this
                        // service.
                        if let Ok(child_resolved_state) = child_instance.lock_resolved_state().await
                        {
                            if child_resolved_state
                                .sandbox
                                .component_output
                                .capabilities()
                                .get(&self.service_name)
                                .ok()
                                .flatten()
                                .is_some()
                            {
                                instances.push(AggregateInstance::Child(child_name));
                            }
                        }
                    }
                }
            }
        }
        Ok(instances)
    }

    async fn route_instance(
        &self,
        instance: &AggregateInstance,
    ) -> Result<(Router<DirEntry>, CapabilitySource), RoutingError> {
        let maybe_router = self
            .sources
            .iter()
            .filter_map(|source| match source {
                AggregateSource::DirectoryRouter { source_instance, router }
                    if instance == source_instance =>
                {
                    Some(router.clone())
                }
                _ => None,
            })
            .next();
        let router = match maybe_router {
            Some(router) => router,
            None => {
                // Both of the panics below should never be reached. For self, parent, and
                // statically declared children `build_component_sandbox` will create a source of
                // `AggregateSource::DirectoryRouter`. If we are asked to route an instance for any
                // of these and there is not a `DirectoryRouter`, then there's a bug in the sandbox
                // construction code.
                let child_name = match instance {
                    AggregateInstance::Child(name) => name,
                    AggregateInstance::Parent | AggregateInstance::Self_ => {
                        panic!("found a parent or self in a collection? this is impossible, and surely a bug");
                    }
                };
                if child_name.collection.is_none() {
                    panic!("found a static child in a collection? this is impossible, and surely a bug");
                }
                let child_instance = {
                    let component = self.component.upgrade()?;
                    let resolved_state = component.lock_resolved_state().await.map_err(|_| {
                        RoutingError::offer_from_child_instance_not_found(
                            child_name,
                            &self.component.moniker,
                            self.service_name.as_str(),
                        )
                    })?;
                    resolved_state
                        .get_child(child_name)
                        .ok_or_else(|| {
                            RoutingError::offer_from_child_instance_not_found(
                                child_name,
                                &self.component.moniker,
                                self.service_name.as_str(),
                            )
                        })?
                        .clone()
                };
                let child_resolved_state =
                    child_instance.lock_resolved_state().await.map_err(|_| {
                        RoutingError::offer_from_child_instance_not_found(
                            child_name,
                            &self.component.moniker,
                            self.service_name.as_str(),
                        )
                    })?;
                let capability = child_resolved_state
                    .sandbox
                    .component_output
                    .capabilities()
                    .get(&self.service_name)
                    .ok()
                    .flatten()
                    .ok_or_else(|| {
                        RoutingError::expose_from_child_expose_not_found(
                            child_name,
                            &self.component.moniker,
                            self.service_name.as_str(),
                        )
                    })?;
                match capability {
                    Capability::DirEntryRouter(r) => r,
                    other_type => {
                        return Err(RoutingError::BedrockWrongCapabilityType {
                            actual: format!("{:?}", other_type),
                            expected: "Router<DirEntry>".to_string(),
                            moniker: self.component.moniker.clone().into(),
                        })
                    }
                }
            }
        };
        match router.route(None, true).await? {
            RouterResponse::Debug(data) => Ok((
                router,
                data.try_into().expect("failed to convert capability source data to struct"),
            )),
            RouterResponse::Unavailable => Err(RoutingError::RouteUnexpectedUnavailable {
                type_name: cm_rust::CapabilityTypeName::Service,
                moniker: self.component.moniker.clone().into(),
            }),
            // We won't see `RouterResponse::Capability` because the debug flag is set to `true` in
            // the call to `route`.
            RouterResponse::Capability(_) => {
                panic!("received capability when requesting debug info")
            }
        }
    }
}
