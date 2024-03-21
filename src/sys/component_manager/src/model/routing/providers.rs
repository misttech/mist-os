// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{
        capability::CapabilityProvider,
        model::{
            component::{StartReason, WeakComponentInstance},
            error::{CapabilityProviderError, ComponentProviderError},
            hooks::{CapabilityReceiver, Event, EventPayload},
            start::Start,
        },
    },
    ::routing::path::PathBufExt,
    async_trait::async_trait,
    clonable_error::ClonableError,
    cm_types::{Name, Path},
    cm_util::{channel, TaskGroup},
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    sandbox::Open,
    std::{path::PathBuf, sync::Arc},
    vfs::{directory::entry_container::Directory, execution_scope::ExecutionScope},
};

/// The default provider for a ComponentCapability.
/// This provider will start the source component instance and open the capability `name` at
/// `path` under the source component's outgoing namespace.
pub struct DefaultComponentCapabilityProvider {
    target: WeakComponentInstance,
    source: WeakComponentInstance,
    name: Name,
    path: Path,
}

impl DefaultComponentCapabilityProvider {
    pub fn new(
        target: WeakComponentInstance,
        source: WeakComponentInstance,
        name: Name,
        path: Path,
    ) -> Self {
        DefaultComponentCapabilityProvider { target, source, name, path }
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
        // Start the source component, if necessary.
        let source = self.source.upgrade()?;
        source
            .ensure_started(&StartReason::AccessCapability {
                target: self.target.moniker.clone(),
                name: self.name.clone(),
            })
            .await
            .map_err(Into::<ComponentProviderError>::into)?;

        // Send a CapabilityRequested event.
        let (receiver, sender) = CapabilityReceiver::new();
        let event = Event::new(
            &self.target.upgrade()?,
            EventPayload::CapabilityRequested {
                source_moniker: source.moniker.clone(),
                name: self.name.to_string(),
                receiver: receiver.clone(),
            },
        );
        source.hooks.dispatch(&event).await;

        // If a component intercepts the capability request through hooks, then
        // send them the channel.
        if receiver.is_taken() {
            let open: Open = sender.into();
            open.open(
                ExecutionScope::new(),
                flags,
                vfs::path::Path::dot(),
                channel::take_channel(server_end),
            );
            return Ok(());
        }

        // No request hooks, so lets send to the outgoing directory.
        let path = self.path.to_path_buf().attach(relative_path);
        let path = path.to_str().ok_or(CapabilityProviderError::BadPath)?;
        source
            .open_outgoing(flags, path, server_end)
            .map_err(|e| CapabilityProviderError::ComponentProviderError { err: e.into() })?;
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
