// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{component::ComponentInstance, error::CapabilityProviderError},
    async_trait::async_trait,
    cm_util::channel,
    cm_util::TaskGroup,
    fidl::endpoints::{ProtocolMarker, ServerEnd},
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    std::path::PathBuf,
    std::sync,
    vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope},
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
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), CapabilityProviderError>;
}

/// A trait for framework capabilities. This trait provides an implementation of
/// [CapabilityProvider::open] that wraps the `open` in a vfs service, ensuring that the capability is
/// fully fuchsia.io-compliant.
#[async_trait]
pub trait FrameworkCapabilityProvider: Send + Sync {
    type Marker: ProtocolMarker;

    /// Binds a server end of a zx::Channel to the provided capability, which is assumed to be a
    /// protocol capability.
    async fn open_protocol(self: Box<Self>, server_end: ServerEnd<Self::Marker>);
}

#[async_trait]
impl<T: FrameworkCapabilityProvider + 'static> CapabilityProvider for T {
    async fn open(
        self: Box<Self>,
        task_group: TaskGroup,
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), CapabilityProviderError> {
        let this = sync::Mutex::new(Some(self));
        let service = vfs::service::endpoint(
            move |_scope: ExecutionScope, server_end: fuchsia_async::Channel| {
                let mut this = this.lock().unwrap();
                let this = this.take().expect("vfs open shouldn't be called more than once");
                let server_end: ServerEnd<<Self as FrameworkCapabilityProvider>::Marker> =
                    server_end.into_zx_channel().into();
                task_group.spawn(this.open_protocol(server_end));
            },
        );
        let relative_path = match relative_path.to_string_lossy() {
            s if s.is_empty() => vfs::path::Path::dot(),
            s => vfs::path::Path::validate_and_split(s)
                .map_err(|_| CapabilityProviderError::BadPath)?,
        };
        let server_end = channel::take_channel(server_end);
        service.open(ExecutionScope::new(), flags, relative_path, server_end.into());
        Ok(())
    }
}
