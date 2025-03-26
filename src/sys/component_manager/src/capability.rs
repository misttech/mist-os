// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::{ComponentInstance, WeakComponentInstance};
use ::routing::capability_source::InternalCapability;
use ::routing::component_instance::ComponentInstanceInterface;
use async_trait::async_trait;
use cm_util::TaskGroup;
use errors::CapabilityProviderError;
use fidl::handle::Channel;
use fidl_fuchsia_io as fio;
use std::sync;
use std::sync::Arc;
use vfs::directory::entry::OpenRequest;
use vfs::execution_scope::ExecutionScope;
use vfs::ToObjectRequest;

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

/// Opens a connection to a [FrameworkCapability] with scope `instance`. Useful for opening a
/// standalone connection to the capability outside the context of routing.
pub async fn open_framework(
    this: &impl FrameworkCapability,
    instance: &Arc<ComponentInstance>,
    server: Channel,
) -> Result<(), CapabilityProviderError> {
    let task_group = instance.nonblocking_task_group();
    let weak_instance = instance.as_weak();
    const FLAGS: fio::Flags = fio::Flags::PROTOCOL_SERVICE;
    let mut object_request = FLAGS.to_object_request(server);
    let open_request = OpenRequest::new(
        instance.execution_scope.clone(),
        FLAGS,
        vfs::path::Path::dot(),
        &mut object_request,
    );
    this.new_provider(weak_instance.clone(), weak_instance).open(task_group, open_request).await
}
