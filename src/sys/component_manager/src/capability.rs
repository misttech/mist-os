// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::{ComponentInstance, WeakComponentInstance},
        error::CapabilityProviderError,
    },
    ::routing::capability_source::{ComponentCapability, InternalCapability},
    async_trait::async_trait,
    cm_util::TaskGroup,
    fuchsia_zircon as zx,
    std::sync,
    vfs::{directory::entry::OpenRequest, execution_scope::ExecutionScope},
};

pub type CapabilitySource = ::routing::capability_source::CapabilitySource<ComponentInstance>;

/// The server-side of a capability implements this trait.
/// Multiple `CapabilityProvider` objects can compose with one another for a single
/// capability request. For example, a `CapabilityProvider` can be interposed
/// between the primary `CapabilityProvider and the client for the purpose of
/// logging and testing. A `CapabilityProvider` is typically provided by a
/// corresponding `Hook` in response to the `CapabilityRouted` event.
/// A capability provider is used exactly once as a result of exactly one route.
#[async_trait]
pub trait CapabilityProvider: Send + Sync {
    /// Binds a server end of a zx::Channel to the provided capability.  If the capability is a
    /// directory, then `flags`, and `relative_path` will be propagated along to open
    /// the appropriate directory.
    async fn open(
        self: Box<Self>,
        task_group: TaskGroup,
        open_request: OpenRequest<'_>,
    ) -> Result<(), CapabilityProviderError>;
}

/// A trait for builtin and framework capabilities. This trait provides an implementation of
/// [CapabilityProvider::open] that wraps the `open` in a vfs service, ensuring that the capability is
/// fully fuchsia.io-compliant.
#[async_trait]
pub trait InternalCapabilityProvider: Send + Sync {
    /// Binds a server end of a zx::Channel to the provided capability, which is assumed to be a
    /// protocol capability.
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel);
}

#[async_trait]
impl<T: InternalCapabilityProvider + 'static> CapabilityProvider for T {
    async fn open(
        self: Box<Self>,
        task_group: TaskGroup,
        open_request: OpenRequest<'_>,
    ) -> Result<(), CapabilityProviderError> {
        let this = sync::Mutex::new(Some(self));
        let service = vfs::service::endpoint(
            move |_scope: ExecutionScope, server_end: fuchsia_async::Channel| {
                let mut this = this.lock().unwrap();
                let this = this.take().expect("vfs open shouldn't be called more than once");
                task_group.spawn(this.open_protocol(server_end.into_zx_channel()));
            },
        );
        open_request.open_service(service).map_err(|e| CapabilityProviderError::VfsOpenError(e))
    }
}

/// Builtin capabilities implement this trait to register themselves with component manager's
/// builtin environment.
pub trait BuiltinCapability: Send + Sync {
    /// Returns true if `capability` matches this framework capability.
    fn matches(&self, capability: &InternalCapability) -> bool;

    /// Returns a [CapabilityProvider] that serves this builtin capability and was
    /// requested by `target`.
    fn new_provider(&self, target: WeakComponentInstance) -> Box<dyn CapabilityProvider>;
}

/// Framework capabilities implement this trait to register themselves with component manager's
/// builtin environment.
pub trait FrameworkCapability: Send + Sync {
    /// Returns true if `capability` matches this framework capability.
    fn matches(&self, capability: &InternalCapability) -> bool;

    /// Returns a [CapabilityProvider] that serves this framework capability with `scope`
    /// and was requested by `target`.
    fn new_provider(
        &self,
        scope: WeakComponentInstance,
        target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider>;
}

/// This trait is implemented by capabilities that are derived from other capabilities.
#[async_trait]
pub trait DerivedCapability: Send + Sync {
    /// Returns a [CapabilityProvider] that serves this derived capability with `scope`
    /// if `source_capability` matches, or `None` otherwise.
    async fn maybe_new_provider(
        &self,
        source_capability: &ComponentCapability,
        scope: WeakComponentInstance,
    ) -> Option<Box<dyn CapabilityProvider>>;
}
