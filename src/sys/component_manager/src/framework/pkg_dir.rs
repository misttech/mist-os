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
    cm_util::channel,
    cm_util::TaskGroup,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    std::path::PathBuf,
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
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
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
        let package = resolved_state.package().cloned();

        let relative_path =
            relative_path.to_str().ok_or(CapabilityProviderError::BadPath)?.to_string();
        let server_end = ServerEnd::new(channel::take_channel(server_end));
        if let Some(package) = package {
            if relative_path.is_empty() {
                package
                    .package_dir
                    .clone(flags, server_end)
                    .map_err(|err| PkgDirError::OpenFailed { err })?;
            } else {
                package
                    .package_dir
                    .open(flags, fio::ModeType::empty(), &relative_path, server_end)
                    .map_err(|err| PkgDirError::OpenFailed { err })?;
            }
        } else {
            return Err(CapabilityProviderError::PkgDirError { err: PkgDirError::NoPkgDir });
        }
        Ok(())
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
