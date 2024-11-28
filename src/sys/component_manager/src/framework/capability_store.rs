// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::{CapabilityProvider, FrameworkCapability, InternalCapabilityProvider};
use crate::model::component::WeakComponentInstance;
use ::routing::capability_source::InternalCapability;
use async_trait::async_trait;
use cm_types::Name;
use fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd};
use fidl_fuchsia_component_sandbox as fsandbox;
use lazy_static::lazy_static;
use tracing::*;

lazy_static! {
    static ref CAPABILITY_NAME: Name =
        fsandbox::CapabilityStoreMarker::PROTOCOL_NAME.parse().unwrap();
}

struct CapabilityStoreCapabilityProvider {}

#[async_trait]
impl InternalCapabilityProvider for CapabilityStoreCapabilityProvider {
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel) {
        let server_end = ServerEnd::<fsandbox::CapabilityStoreMarker>::new(server_end);
        // We only need to look up the component matching this scope.
        // These operations should all work, even if the component is not running.
        let serve_result = self.serve(server_end.into_stream()).await;
        if let Err(error) = serve_result {
            warn!(%error, "CapabilityStore serve failed");
        }
    }
}

impl CapabilityStoreCapabilityProvider {
    async fn serve(
        &self,
        stream: fsandbox::CapabilityStoreRequestStream,
    ) -> Result<(), fidl::Error> {
        sandbox::serve_capability_store(stream).await
    }
}

#[derive(Debug, Clone)]
pub struct CapabilityStore {}

impl CapabilityStore {
    pub fn new() -> Self {
        Self {}
    }
}

impl FrameworkCapability for CapabilityStore {
    fn matches(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&CAPABILITY_NAME)
    }

    fn new_provider(
        &self,
        _scope: WeakComponentInstance,
        _target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        Box::new(CapabilityStoreCapabilityProvider {})
    }
}
