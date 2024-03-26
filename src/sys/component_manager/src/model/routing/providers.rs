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
    ::routing::{error::RoutingError, path::PathBufExt},
    async_trait::async_trait,
    bedrock_error::BedrockError,
    clonable_error::ClonableError,
    cm_rust::Availability,
    cm_types::Name,
    cm_util::{channel, TaskGroup},
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    std::{iter, path::PathBuf, sync::Arc},
    vfs::{
        directory::{entry::OpenRequest, entry_container::Directory},
        execution_scope::ExecutionScope,
        ToObjectRequest,
    },
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
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), CapabilityProviderError> {
        let source = self.source.upgrade()?;
        let path = cm_util::io::path_buf_to_vfs_path(relative_path)
            .ok_or(CapabilityProviderError::BadPath)?;
        let capability = source
            .get_program_output_dict()
            .await?
            .get_with_request(
                iter::once(&self.name),
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
        let server_end = cm_util::channel::take_channel(server_end);
        flags.to_object_request(server_end).handle(|object_request| {
            entry.clone().open_entry(OpenRequest::new(
                ExecutionScope::new(),
                flags,
                path,
                object_request,
            ))
        });
        Ok(())
    }
}

/// The default provider for a Namespace Capability.
pub struct NamespaceCapabilityProvider {
    pub path: cm_types::Path,
}

#[async_trait]
impl CapabilityProvider for NamespaceCapabilityProvider {
    async fn open(
        self: Box<Self>,
        _task_group: TaskGroup,
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), CapabilityProviderError> {
        let namespace_path = self.path.to_path_buf().attach(relative_path);
        let namespace_path = namespace_path.to_str().ok_or(CapabilityProviderError::BadPath)?;
        let server_end = channel::take_channel(server_end);
        fuchsia_fs::node::open_channel_in_namespace(
            namespace_path,
            flags,
            ServerEnd::new(server_end),
        )
        .map_err(|e| CapabilityProviderError::CmNamespaceError {
            err: ClonableError::from(anyhow::Error::from(e)),
        })
    }
}

/// A `CapabilityProvider` that serves a pseudo directory entry.
#[derive(Clone)]
pub struct DirectoryEntryCapabilityProvider {
    /// Execution scope for requests to `entry`.
    pub execution_scope: ExecutionScope,

    /// The pseudo directory that backs this capability.
    pub entry: Arc<vfs::directory::immutable::simple::Simple>,
}

#[async_trait]
impl CapabilityProvider for DirectoryEntryCapabilityProvider {
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

        self.entry.open(
            self.execution_scope.clone(),
            flags,
            relative_path,
            ServerEnd::new(channel::take_channel(server_end)),
        );

        Ok(())
    }
}
