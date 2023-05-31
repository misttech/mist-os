// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{
        capability::CapabilityProvider,
        model::{
            component::{ComponentInstance, StartReason, WeakComponentInstance},
            error::{CapabilityProviderError, ComponentProviderError},
            hooks::{Event, EventPayload},
        },
    },
    ::routing::path::PathBufExt,
    async_trait::async_trait,
    clonable_error::ClonableError,
    cm_rust::{self, CapabilityPath},
    cm_task_scope::TaskScope,
    cm_types::Name,
    cm_util::channel,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::lock::Mutex,
    std::{path::PathBuf, sync::Arc},
    vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope},
};

/// The default provider for a ComponentCapability.
/// This provider will start the source component instance and open the capability `name` at
/// `path` under the source component's outgoing namespace.
pub struct DefaultComponentCapabilityProvider {
    pub target: WeakComponentInstance,
    pub source: WeakComponentInstance,
    pub name: Name,
    pub path: CapabilityPath,
}

#[async_trait]
impl CapabilityProvider for DefaultComponentCapabilityProvider {
    async fn open(
        self: Box<Self>,
        _task_scope: TaskScope,
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), CapabilityProviderError> {
        let capability = Arc::new(Mutex::new(Some(channel::take_channel(server_end))));
        let res = async {
            // Start the source component, if necessary
            let source = self
                .source
                .upgrade()
                .map_err(|_| ComponentProviderError::SourceInstanceNotFound)?;
            source
                .start(&StartReason::AccessCapability {
                    target: self.target.abs_moniker.clone(),
                    name: self.name.clone(),
                })
                .await?;

            let event = Event::new(
                &self
                    .target
                    .upgrade()
                    .map_err(|_| ComponentProviderError::TargetInstanceNotFound)?,
                EventPayload::CapabilityRequested {
                    source_moniker: source.abs_moniker.clone(),
                    name: self.name.to_string(),
                    capability: capability.clone(),
                },
            );
            source.hooks.dispatch(&event).await;
            Result::<Arc<ComponentInstance>, ComponentProviderError>::Ok(source)
        }
        .await;

        // If the capability transported through the event above wasn't transferred
        // out, then we can open the capability through the component's outgoing directory.
        // If some hook consumes the capability, then we don't bother looking in the outgoing
        // directory.
        let capability = capability.lock().await.take();
        if let Some(mut server_end_in) = capability {
            // Pass back the channel so the caller can set the epitaph, if necessary.
            *server_end = channel::take_channel(&mut server_end_in);
            let path = self.path.to_path_buf().attach(relative_path);
            let path = path.to_str().ok_or(CapabilityProviderError::BadPath)?;
            res?.open_outgoing(flags, path, server_end)
                .await
                .map_err(|e| CapabilityProviderError::ComponentProviderError { err: e.into() })?;
        } else {
            res?;
        }
        Ok(())
    }
}

/// The default provider for a Namespace Capability.
pub struct NamespaceCapabilityProvider {
    pub path: CapabilityPath,
}

#[async_trait]
impl CapabilityProvider for NamespaceCapabilityProvider {
    async fn open(
        self: Box<Self>,
        _task_scope: TaskScope,
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

    /// The pseudo directory entry that backs this capability.
    pub entry: Arc<dyn DirectoryEntry>,
}

#[async_trait]
impl CapabilityProvider for DirectoryEntryCapabilityProvider {
    async fn open(
        self: Box<Self>,
        _task_scope: TaskScope,
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
