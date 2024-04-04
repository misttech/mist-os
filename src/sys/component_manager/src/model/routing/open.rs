// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::{CapabilityProvider, CapabilitySource},
        model::{
            component::{ComponentInstance, ExtendedInstance, StartReason, WeakComponentInstance},
            error::{CapabilityProviderError, ModelError, OpenError},
            routing::{
                providers::{
                    DefaultComponentCapabilityProvider, DirectoryEntryCapabilityProvider,
                    NamespaceCapabilityProvider,
                },
                service::{
                    AnonymizedAggregateServiceDir, AnonymizedServiceRoute,
                    FilteredAggregateServiceProvider,
                },
                RouteSource,
            },
            start::Start,
            storage::{self, BackingDirectoryInfo},
        },
    },
    ::routing::component_instance::ComponentInstanceInterface,
    cm_moniker::InstancedExtendedMoniker,
    fidl_fuchsia_io as fio,
    moniker::MonikerBase,
    std::sync::Arc,
    vfs::{directory::entry::OpenRequest, remote::remote_dir},
};

/// A request to open a capability at its source.
pub enum CapabilityOpenRequest<'a> {
    // Open a capability backed by a component's outgoing directory.
    OutgoingDirectory {
        open_request: OpenRequest<'a>,
        source: CapabilitySource,
        target: &'a Arc<ComponentInstance>,
    },
    // Open a storage capability.
    Storage {
        open_request: OpenRequest<'a>,
        source: storage::BackingDirectoryInfo,
        target: &'a Arc<ComponentInstance>,
    },
}

impl<'a> CapabilityOpenRequest<'a> {
    /// Creates a request to open a capability with source `route_source` for `target`.
    pub fn new_from_route_source(
        route_source: RouteSource,
        target: &'a Arc<ComponentInstance>,
        mut open_request: OpenRequest<'a>,
    ) -> Result<Self, ModelError> {
        let RouteSource { source, relative_path } = route_source;
        if !relative_path.is_dot() {
            open_request.prepend_path(
                &relative_path.to_string().try_into().map_err(|_| ModelError::BadPath)?,
            );
        }
        Ok(Self::OutgoingDirectory { open_request, source, target })
    }

    /// Creates a request to open a storage capability with source `storage_source` for `target`.
    pub fn new_from_storage_source(
        source: BackingDirectoryInfo,
        target: &'a Arc<ComponentInstance>,
        open_request: OpenRequest<'a>,
    ) -> Self {
        Self::Storage { open_request, source, target }
    }

    /// Opens the capability in `self`, triggering a `CapabilityRouted` event and binding
    /// to the source component instance if necessary.
    pub async fn open(self) -> Result<(), OpenError> {
        match self {
            Self::OutgoingDirectory { open_request, source, target } => {
                Self::open_outgoing_directory(open_request, source, target).await
            }
            Self::Storage { open_request, source, target } => {
                Self::open_storage(open_request, &source, target)
                    .await
                    .map_err(|e| OpenError::OpenStorageError { err: Box::new(e) })
            }
        }
    }

    async fn open_outgoing_directory(
        mut open_request: OpenRequest<'a>,
        source: CapabilitySource,
        target: &Arc<ComponentInstance>,
    ) -> Result<(), OpenError> {
        let capability_provider = if let Some(provider) =
            Self::get_default_provider(target.as_weak(), &source)
                .await
                .map_err(|e| OpenError::GetDefaultProviderError { err: Box::new(e) })?
        {
            provider
        } else {
            target
                .context
                .find_internal_provider(&source, target.as_weak())
                .await
                .ok_or(OpenError::CapabilityProviderNotFound)?
        };

        let source_instance =
            source.source_instance().upgrade().map_err(CapabilityProviderError::from)?;
        let task_group = match source_instance {
            ExtendedInstance::AboveRoot(top) => top.task_group(),
            ExtendedInstance::Component(component) => {
                open_request.set_scope(component.execution_scope.clone());
                component.nonblocking_task_group()
            }
        };
        capability_provider.open(task_group, open_request).await?;
        Ok(())
    }

    async fn open_storage(
        open_request: OpenRequest<'a>,
        source: &storage::BackingDirectoryInfo,
        target: &Arc<ComponentInstance>,
    ) -> Result<(), ModelError> {
        // As of today, the storage component instance must contain the target. This is because it
        // is impossible to expose storage declarations up.
        let moniker =
            target.instanced_moniker().strip_prefix(&source.storage_source_moniker).unwrap();

        let dir_source = source.storage_provider.clone();
        let storage_dir_proxy = storage::open_isolated_storage(
            &source,
            target.persistent_storage,
            moniker.clone(),
            target.instance_id(),
        )
        .await
        .map_err(|e| ModelError::from(e))?;

        open_request.open_remote(remote_dir(storage_dir_proxy)).map_err(|err| {
            let source_moniker = match &dir_source {
                Some(r) => {
                    InstancedExtendedMoniker::ComponentInstance(r.instanced_moniker().clone())
                }
                None => InstancedExtendedMoniker::ComponentManager,
            };
            ModelError::OpenStorageFailed { source_moniker, moniker, path: String::new(), err }
        })?;
        Ok(())
    }

    /// Returns an instance of the default capability provider for the capability at `source`, if
    /// supported.
    async fn get_default_provider(
        target: WeakComponentInstance,
        source: &CapabilitySource,
    ) -> Result<Option<Box<dyn CapabilityProvider>>, ModelError> {
        match source {
            CapabilitySource::Component { capability, component } => {
                // Route normally for a component capability with a source path
                Ok(match capability.source_path() {
                    Some(_) => Some(Box::new(DefaultComponentCapabilityProvider::new(
                        target,
                        component.clone(),
                        capability
                            .source_name()
                            .expect("capability with source path should have a name")
                            .clone(),
                    ))),
                    _ => None,
                })
            }
            CapabilitySource::Namespace { capability, .. } => match capability.source_path() {
                Some(path) => Ok(Some(Box::new(NamespaceCapabilityProvider {
                    path: path.clone(),
                    is_directory_like: fio::DirentType::from(capability.type_name())
                        == fio::DirentType::Directory,
                }))),
                _ => Ok(None),
            },
            CapabilitySource::FilteredAggregate { capability_provider, component, .. } => {
                // TODO(https://fxbug.dev/42124541): This should cache the directory
                Ok(Some(Box::new(
                    FilteredAggregateServiceProvider::new(
                        component.clone(),
                        target,
                        capability_provider.clone(),
                    )
                    .await?,
                )))
            }
            CapabilitySource::AnonymizedAggregate {
                capability,
                component,
                aggregate_capability_provider,
                members,
            } => {
                let source_component_instance = component.upgrade()?;

                let route = AnonymizedServiceRoute {
                    source_moniker: source_component_instance.moniker.clone(),
                    members: members.clone(),
                    service_name: capability.source_name().clone(),
                };

                source_component_instance
                    .ensure_started(&StartReason::AccessCapability {
                        target: target.moniker.clone(),
                        name: capability.source_name().clone(),
                    })
                    .await?;

                // If there is an existing collection service directory, provide it.
                {
                    let state = source_component_instance.lock_resolved_state().await?;
                    if let Some(service_dir) = state.anonymized_services.get(&route) {
                        let provider = DirectoryEntryCapabilityProvider {
                            entry: service_dir.dir_entry().await,
                        };
                        return Ok(Some(Box::new(provider)));
                    }
                }

                // Otherwise, create one. This must be done while the component ResolvedInstanceState
                // is unlocked because the AggregateCapabilityProvider uses locked state.
                let service_dir = Arc::new(AnonymizedAggregateServiceDir::new(
                    component.clone(),
                    route.clone(),
                    aggregate_capability_provider.clone_boxed(),
                ));

                source_component_instance.hooks.install(service_dir.hooks()).await;

                let provider = {
                    let mut state = source_component_instance.lock_resolved_state().await?;
                    let entry = service_dir.dir_entry().await;

                    state.anonymized_services.insert(route, service_dir.clone());

                    DirectoryEntryCapabilityProvider { entry }
                };

                // Populate the service dir with service entries from children that may have been started before the service
                // capability had been routed from the collection.
                service_dir.add_entries_from_children().await?;

                Ok(Some(Box::new(provider)))
            }
            // These capabilities do not have a default provider.
            CapabilitySource::Framework { .. }
            | CapabilitySource::Void { .. }
            | CapabilitySource::Capability { .. }
            | CapabilitySource::Builtin { .. }
            | CapabilitySource::Environment { .. } => Ok(None),
        }
    }
}
