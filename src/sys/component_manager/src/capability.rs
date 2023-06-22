// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{component::ComponentInstance, error::CapabilityProviderError},
    async_trait::async_trait,
    cm_task_scope::TaskScope,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    std::path::PathBuf,
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
        task_scope: TaskScope,
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), CapabilityProviderError>;
}

/// The only flags that are accepted by `CapabilityProvider` implementations.
pub const PERMITTED_FLAGS: fio::OpenFlags = fio::OpenFlags::empty()
    .union(fio::OpenFlags::RIGHT_READABLE)
    .union(fio::OpenFlags::RIGHT_WRITABLE)
    .union(fio::OpenFlags::POSIX_WRITABLE)
    .union(fio::OpenFlags::DIRECTORY)
    .union(fio::OpenFlags::NOT_DIRECTORY);
