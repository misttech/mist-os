// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module implements the ability to use, offer, or expose the `pkg` directory with a source
//! of `framework`.

use {
    crate::{
        capability::{CapabilityProvider, FrameworkCapability},
        model::{
            component::WeakComponentInstance,
            error::{CapabilityProviderError, PkgDirError},
        },
    },
    ::routing::{capability_source::InternalCapability, error::ComponentInstanceError},
    async_trait::async_trait,
    cm_util::TaskGroup,
    vfs::{directory::entry::OpenRequest, remote::remote_dir},
};

struct PkgDirectoryProvider {
    scope: WeakComponentInstance,
}

impl PkgDirectoryProvider {
    pub fn new(scope: WeakComponentInstance) -> Self {
        PkgDirectoryProvider { scope }
    }
}

#[async_trait]
impl CapabilityProvider for PkgDirectoryProvider {
    async fn open(
        self: Box<Self>,
        _task_group: TaskGroup,
        open_request: OpenRequest<'_>,
    ) -> Result<(), CapabilityProviderError> {
        let component = self.scope.upgrade().map_err(|_| {
            ComponentInstanceError::InstanceNotFound { moniker: self.scope.moniker.clone() }
        })?;
        let resolved_state = component.lock_resolved_state().await.map_err(|err| {
            let err: anyhow::Error = err.into();
            ComponentInstanceError::ResolveFailed {
                moniker: self.scope.moniker.clone(),
                err: err.into(),
            }
        })?;

        if let Some(package) = resolved_state.package().as_ref() {
            open_request
                .open_remote(remote_dir(Clone::clone(&package.package_dir)))
                .map_err(|e| CapabilityProviderError::VfsOpenError(e))
        } else {
            Err(CapabilityProviderError::PkgDirError { err: PkgDirError::NoPkgDir })
        }
    }
}

pub struct PkgDirectoryFrameworkCapability;

impl PkgDirectoryFrameworkCapability {
    pub fn new() -> Self {
        Self {}
    }
}

impl FrameworkCapability for PkgDirectoryFrameworkCapability {
    fn matches(&self, capability: &InternalCapability) -> bool {
        matches!(capability, InternalCapability::Directory(n) if n.as_str() == "pkg")
    }

    fn new_provider(
        &self,
        scope: WeakComponentInstance,
        _target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        Box::new(PkgDirectoryProvider::new(scope))
    }
}
