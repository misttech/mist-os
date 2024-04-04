// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    super::router::Request,
    crate::{
        capability::CapabilityProvider,
        model::{
            component::WeakComponentInstance,
            error::{CapabilityProviderError, OpenError},
        },
        sandbox_util::DictExt,
    },
    ::routing::error::RoutingError,
    async_trait::async_trait,
    bedrock_error::BedrockError,
    clonable_error::ClonableError,
    cm_rust::Availability,
    cm_types::{Name, OPEN_FLAGS_MAX_POSSIBLE_RIGHTS},
    cm_util::TaskGroup,
    std::sync::Arc,
    vfs::{directory::entry::OpenRequest, path::Path as VfsPath, remote::remote_dir},
};

/// The default provider for a ComponentCapability.
/// This provider will start the source component instance and open the capability `name` at
/// `path` under the source component's outgoing namespace.
pub struct DefaultComponentCapabilityProvider {
    target: WeakComponentInstance,
    source: WeakComponentInstance,
    name: Name,
}

impl DefaultComponentCapabilityProvider {
    pub fn new(target: WeakComponentInstance, source: WeakComponentInstance, name: Name) -> Self {
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
        let source = self.source.upgrade()?;
        let capability = source
            .get_program_output_dict()
            .await?
            .get_with_request(
                &self.name,
                // Routers in `program_output_dict` do not check availability but we need a
                // request to run hooks.
                Request { availability: Availability::Transitional, target: self.target.clone() },
            )
            .await?
            .ok_or_else(|| RoutingError::BedrockNotPresentInDictionary {
                name: self.name.to_string(),
            })
            .map_err(BedrockError::from)?;
        let entry = capability
            .try_into_directory_entry()
            .map_err(OpenError::DoesNotSupportOpen)
            .map_err(BedrockError::from)?;
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
                fuchsia_fs::directory::open_in_namespace(dir, OPEN_FLAGS_MAX_POSSIBLE_RIGHTS)
                    .map_err(|e| CapabilityProviderError::CmNamespaceError {
                        err: ClonableError::from(anyhow::Error::from(e)),
                    })?,
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
