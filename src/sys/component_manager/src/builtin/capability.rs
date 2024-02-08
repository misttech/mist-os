// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Error},
    async_trait::async_trait,
    fidl::endpoints::ProtocolMarker,
    fuchsia_zircon::{self as zx, ResourceInfo},
    routing::capability_source::InternalCapability,
    std::sync::Arc,
};

/// A builtin capability, whether it be a `framework` capability
/// or a capability that originates above the root realm (from component_manager).
///
/// Implementing this trait takes care of certain boilerplate, like registering
/// event hooks and the creation of a CapabilityProvider.
#[async_trait]
pub trait BuiltinCapability {
    /// Name of the capability. Used for hook registration and logging.
    const NAME: &'static str;

    /// Service marker for the capability.
    type Marker: ProtocolMarker;

    /// Serves an instance of the capability given an appropriate RequestStream.
    /// Returns when the channel backing the RequestStream is closed or an
    /// unrecoverable error occurs.
    #[allow(dead_code)]
    async fn serve(
        self: Arc<Self>,
        mut stream: <Self::Marker as ProtocolMarker>::RequestStream,
    ) -> Result<(), Error>;

    /// Returns true if the builtin capability matches the requested `capability`
    /// and should be served.
    #[allow(dead_code)]
    fn matches_routed_capability(&self, capability: &InternalCapability) -> bool;
}

#[async_trait]
pub trait ResourceCapability {
    /// The kind of resource.
    const KIND: zx::sys::zx_rsrc_kind_t;

    /// Name of the capability. Used for hook registration and logging.
    const NAME: &'static str;

    /// Service marker for the capability.
    type Marker: ProtocolMarker;

    fn get_resource_info(self: &Arc<Self>) -> Result<ResourceInfo, Error>;

    async fn server_loop(
        self: Arc<Self>,
        mut stream: <Self::Marker as ProtocolMarker>::RequestStream,
    ) -> Result<(), Error>;

    fn matches_routed_capability(&self, capability: &InternalCapability) -> bool;
}

#[async_trait]
impl<R: ResourceCapability + Send + Sync> BuiltinCapability for R {
    const NAME: &'static str = R::NAME;
    type Marker = R::Marker;

    async fn serve(
        self: Arc<Self>,
        stream: <R::Marker as ProtocolMarker>::RequestStream,
    ) -> Result<(), Error> {
        let resource_info = self.get_resource_info()?;
        if resource_info.kind != R::KIND || resource_info.base != 0 || resource_info.size != 0 {
            return Err(format_err!("{} not available.", R::NAME));
        }
        self.server_loop(stream).await
    }

    fn matches_routed_capability(&self, capability: &InternalCapability) -> bool {
        self.matches_routed_capability(capability)
    }
}
