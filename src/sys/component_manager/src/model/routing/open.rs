// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::CapabilityProvider;
use crate::model::component::{ComponentInstance, ExtendedInstance, WeakComponentInstance};
use crate::model::routing::providers::{
    DefaultComponentCapabilityProvider, NamespaceCapabilityProvider,
};
use crate::model::storage::{self, BackingDirectoryInfo};
use ::routing::capability_source::{
    AnonymizedAggregateSource, CapabilitySource, ComponentSource, NamespaceSource,
};
use ::routing::component_instance::ComponentInstanceInterface;
use ::routing::RouteSource;
use errors::{CapabilityProviderError, ModelError, OpenError};
use fidl_fuchsia_io as fio;
use moniker::ExtendedMoniker;
use std::sync::Arc;
use vfs::directory::entry::OpenRequest;
use vfs::remote::remote_dir;

#[allow(clippy::large_enum_variant)] // TODO(https://fxbug.dev/401087881)
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
    #[allow(clippy::result_large_err)] // TODO(https://fxbug.dev/401254441)
    /// Creates a request to open a capability with source `route_source` for `target`.
    pub fn new_from_route_source(
        route_source: RouteSource,
        target: &'a Arc<ComponentInstance>,
        mut open_request: OpenRequest<'a>,
    ) -> Result<Self, OpenError> {
        let RouteSource { source, relative_path } = route_source;
        if !relative_path.is_dot() {
            open_request.prepend_path(
                &relative_path.to_string().try_into().map_err(|_| OpenError::BadPath)?,
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

        let source_instance = target
            .find_extended_instance(&source.source_moniker())
            .await
            .map_err(|err| CapabilityProviderError::ComponentInstanceError { err })?;
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
        let moniker = target.moniker().strip_prefix(&source.storage_source_moniker).unwrap();

        let dir_source = source.storage_provider.clone();
        let storage_dir_proxy =
            storage::open_isolated_storage(&source, moniker.clone(), target.instance_id())
                .await
                .map_err(|e| ModelError::from(e))?;

        open_request.open_remote(remote_dir(storage_dir_proxy)).map_err(|err| {
            let source_moniker = match &dir_source {
                Some(r) => ExtendedMoniker::ComponentInstance(r.moniker().clone()),
                None => ExtendedMoniker::ComponentManager,
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
            CapabilitySource::Component(ComponentSource { capability, moniker }) => {
                // Route normally for a component capability with a source path
                Ok(match capability.source_path() {
                    Some(_) => Some(Box::new(DefaultComponentCapabilityProvider::new(
                        target,
                        moniker.clone(),
                        capability
                            .source_name()
                            .expect("capability with source path should have a name")
                            .clone(),
                    ))),
                    _ => None,
                })
            }
            CapabilitySource::Namespace(NamespaceSource { capability, .. }) => {
                match capability.source_path() {
                    Some(path) => Ok(Some(Box::new(NamespaceCapabilityProvider {
                        path: path.clone(),
                        is_directory_like: fio::DirentType::from(capability.type_name())
                            == fio::DirentType::Directory,
                    }))),
                    _ => Ok(None),
                }
            }
            CapabilitySource::FilteredProvider(_)
            | CapabilitySource::FilteredAggregateProvider(_)
            | CapabilitySource::AnonymizedAggregate(AnonymizedAggregateSource { .. }) => {
                // This function should only be used for legacy routing, and these capability
                // sources have been fully moved to bedrock routing.
                panic!("this code should never be reached");
            }
            // These capabilities do not have a default provider.
            CapabilitySource::Framework(_)
            | CapabilitySource::Void(_)
            | CapabilitySource::Capability(_)
            | CapabilitySource::Builtin(_)
            | CapabilitySource::Environment(_) => Ok(None),
        }
    }
}
