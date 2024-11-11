// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::CapabilityProvider;
use crate::model::component::WeakComponentInstance;
use async_trait::async_trait;
use clonable_error::ClonableError;
use cm_rust::{Availability, CapabilityTypeName};
use cm_types::{Name, FLAGS_MAX_POSSIBLE_RIGHTS};
use cm_util::TaskGroup;
use errors::{CapabilityProviderError, OpenError};
use moniker::Moniker;
use router_error::RouterError;
use routing::availability::AvailabilityMetadata;
use routing::bedrock::request_metadata::METADATA_KEY_TYPE;
use routing::component_instance::ComponentInstanceInterface;
use routing::error::{ComponentInstanceError, RoutingError};
use routing::{DictExt, GenericRouterResponse};
use sandbox::{Dict, RemotableCapability, Request};
use std::collections::HashMap;
use std::sync::Arc;
use vfs::directory::entry::OpenRequest;
use vfs::path::Path as VfsPath;
use vfs::remote::remote_dir;

/// The default provider for a ComponentCapability.
/// This provider will start the source component instance and open the capability `name` at
/// `path` under the source component's outgoing namespace.
pub struct DefaultComponentCapabilityProvider {
    target: WeakComponentInstance,
    source: Moniker,
    name: Name,
}

impl DefaultComponentCapabilityProvider {
    pub fn new(target: WeakComponentInstance, source: Moniker, name: Name) -> Self {
        DefaultComponentCapabilityProvider { target, source, name }
    }
}

#[async_trait]
impl CapabilityProvider for DefaultComponentCapabilityProvider {
    async fn open(
        self: Box<Self>,
        _task_group: TaskGroup,
        open_request: OpenRequest<'_>,
    ) -> Result<(), CapabilityProviderError> {
        let source = self.target.upgrade()?.find_absolute(&self.source).await?;
        let caps_with_metadata: HashMap<Name, CapabilityTypeName> = source
            .lock_resolved_state()
            .await
            .map_err(|e| CapabilityProviderError::ComponentInstanceError {
                err: ComponentInstanceError::ResolveFailed {
                    moniker: source.moniker.clone(),
                    err: ClonableError::from(anyhow::anyhow!("{e}")),
                },
            })?
            .decl()
            .capabilities
            .iter()
            .map(|e| (e.name().clone(), CapabilityTypeName::from(e)))
            .collect();
        let metadata = Dict::new();
        let Some(porcelain_type) = caps_with_metadata.get(&self.name) else {
            return Err(RouterError::from(RoutingError::MissingPorcelainType {
                name: self.name,
                moniker: source.moniker.clone(),
            })
            .into());
        };
        metadata
            .insert(
                cm_types::Name::new(METADATA_KEY_TYPE).unwrap(),
                sandbox::Capability::Data(sandbox::Data::String(porcelain_type.to_string())),
            )
            .unwrap();
        metadata.set_availability(Availability::Transitional);
        let resp = source
            .get_program_output_dict()
            .await?
            .get_with_request(
                &source.moniker.clone().into(),
                &self.name,
                // Routers in `program_output_dict` do not check availability but we need a
                // request to run hooks.
                Some(Request { target: self.target.clone().into(), metadata }),
                false,
            )
            .await?
            .ok_or_else(|| RoutingError::BedrockNotPresentInDictionary {
                moniker: self.target.moniker.clone().into(),
                name: self.name.to_string(),
            })?;
        let capability = match resp {
            GenericRouterResponse::Capability(c) => c,
            GenericRouterResponse::Unavailable => {
                return Err(RoutingError::RouteUnexpectedUnavailable {
                    type_name: *porcelain_type,
                    moniker: source.moniker.clone().into(),
                }
                .into());
            }
            GenericRouterResponse::Debug(_) => {
                return Err(RoutingError::RouteUnexpectedDebug {
                    type_name: *porcelain_type,
                    moniker: source.moniker.clone().into(),
                }
                .into())
            }
        };
        let entry = capability
            .try_into_directory_entry(source.execution_scope.clone())
            .map_err(OpenError::DoesNotSupportOpen)
            .map_err(RouterError::from)?;
        entry.open_entry(open_request).map_err(|err| CapabilityProviderError::VfsOpenError(err))
    }
}

/// The default provider for a Namespace Capability.
pub struct NamespaceCapabilityProvider {
    pub path: cm_types::Path,
    pub is_directory_like: bool,
}

#[async_trait]
impl CapabilityProvider for NamespaceCapabilityProvider {
    async fn open(
        self: Box<Self>,
        _task_group: TaskGroup,
        mut open_request: OpenRequest<'_>,
    ) -> Result<(), CapabilityProviderError> {
        let path = self.path.to_path_buf();
        let (dir, base) = if self.is_directory_like {
            (path.to_str().ok_or(CapabilityProviderError::BadPath)?, VfsPath::dot())
        } else {
            (
                match path.parent() {
                    None => "/",
                    Some(p) => p.to_str().ok_or(CapabilityProviderError::BadPath)?,
                },
                path.file_name()
                    .and_then(|f| f.to_str())
                    .ok_or(CapabilityProviderError::BadPath)?
                    .try_into()
                    .map_err(|_| CapabilityProviderError::BadPath)?,
            )
        };

        open_request.prepend_path(&base);

        open_request
            .open_remote(remote_dir(
                fuchsia_fs::directory::open_in_namespace(dir, FLAGS_MAX_POSSIBLE_RIGHTS).map_err(
                    |e| CapabilityProviderError::CmNamespaceError {
                        err: ClonableError::from(anyhow::Error::from(e)),
                    },
                )?,
            ))
            .map_err(|e| CapabilityProviderError::CmNamespaceError {
                err: ClonableError::from(anyhow::Error::from(e)),
            })
    }
}

/// A `CapabilityProvider` that serves a pseudo directory entry.
#[derive(Clone)]
pub struct DirectoryEntryCapabilityProvider {
    /// The pseudo directory that backs this capability.
    pub entry: Arc<vfs::directory::immutable::simple::Simple>,
}

#[async_trait]
impl CapabilityProvider for DirectoryEntryCapabilityProvider {
    async fn open(
        self: Box<Self>,
        _task_group: TaskGroup,
        open_request: OpenRequest<'_>,
    ) -> Result<(), CapabilityProviderError> {
        open_request
            .open_dir(self.entry.clone())
            .map_err(|e| CapabilityProviderError::VfsOpenError(e))
    }
}
