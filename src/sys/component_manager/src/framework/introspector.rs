// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.cti

use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use cm_types::Name;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_component as fcomponent;
use futures::TryStreamExt;
use lazy_static::lazy_static;
use moniker::{Moniker, MonikerBase};
use routing::capability_source::InternalCapability;
use tracing::warn;

use crate::{
    capability::{CapabilityProvider, FrameworkCapability, InternalCapabilityProvider},
    model::{
        component::WeakComponentInstance,
        token::{InstanceRegistry, InstanceToken},
    },
};

lazy_static! {
    static ref INTROSPECTOR_SERVICE: Name = "fuchsia.component.Introspector".parse().unwrap();
}

struct IntrospectorCapability {
    scope_moniker: Moniker,
    instance_registry: Arc<InstanceRegistry>,
}

impl IntrospectorCapability {
    pub fn new(scope_moniker: Moniker, instance_registry: Arc<InstanceRegistry>) -> Self {
        Self { scope_moniker, instance_registry }
    }

    pub async fn serve(
        &self,
        mut stream: fcomponent::IntrospectorRequestStream,
    ) -> Result<(), anyhow::Error> {
        while let Some(request) = stream.try_next().await? {
            let method_name = request.method_name();
            self.handle_request(request)
                .await
                .with_context(|| format!("Error handling Introspector method {method_name}"))?;
        }
        Ok(())
    }

    async fn handle_request(
        &self,
        request: fcomponent::IntrospectorRequest,
    ) -> Result<(), fidl::Error> {
        match request {
            fcomponent::IntrospectorRequest::GetMoniker { component_instance, responder } => {
                let token = InstanceToken::from(component_instance);
                let Some(Ok(moniker)) = self
                    .instance_registry
                    .get(&token)
                    .map(|moniker| moniker.strip_prefix(&self.scope_moniker))
                else {
                    return responder.send(Err(fcomponent::Error::InstanceNotFound));
                };
                return responder.send(Ok(&moniker.to_string()));
            }
            fcomponent::IntrospectorRequest::_UnknownMethod {
                ordinal,
                control_handle: _,
                method_type,
                ..
            } => {
                warn!(%ordinal, "Unknown {method_type:?} Introspector method");
                Ok(())
            }
        }
    }
}

#[async_trait]
impl InternalCapabilityProvider for IntrospectorCapability {
    type Marker = fcomponent::IntrospectorMarker;

    async fn open_protocol(self: Box<Self>, server_end: ServerEnd<Self::Marker>) {
        let serve_result = self.serve(server_end.into_stream().unwrap()).await;
        if let Err(error) = serve_result {
            warn!(%error, "Error serving Introspector");
        }
    }
}

pub struct IntrospectorFrameworkCapability {
    pub instance_registry: Arc<InstanceRegistry>,
}

impl FrameworkCapability for IntrospectorFrameworkCapability {
    fn matches(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&INTROSPECTOR_SERVICE)
    }

    fn new_provider(
        &self,
        scope: WeakComponentInstance,
        _target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        Box::new(IntrospectorCapability::new(scope.moniker.clone(), self.instance_registry.clone()))
    }
}
